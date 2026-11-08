// Talks to JobFinder for the content script on Jobnet: a content script can't
// fetch from another site, the extension can. JobFinder is the desktop app on
// this computer (127.0.0.1:8765, app/ in this repo), which lets this extension
// in by its fixed ID (manifest.json "key"); the "base" in its storage can point
// it at the website instead, with the person's own sign-in there (the host
// permission sends its cookie along).
//
// Messages: {type: "matches"} gives the saved matches as match/history
// does: {matches (newest first), not_joblogged (applied for, not in Jobnet's
// joblog yet)};
// add shortlist: true for the application picker's shortlist in the app's order;
// {type: "joblog", id} the joblog entry of one (match/joblog/{id}), with
// CVR's address when the post has none (withCvrAddress);
// {type: "pdf", id, kind} one of its PDFs as {name, base64};
// {type: "logged", id, status} marks it as entered in Jobnet's joblog
// (match/joblog_status), keeping its status;
// {type: "application_form", id, fields} what goes in the fields of an
// employer's application form (match/application_form), for form-fill.js;
// {type: "applied", id} marks a match as applied for (match/applied), answering
// {status, applied_on};
// {type: "searches"} the job searches a post can be sent to (job_scan/search_names);
// {type: "send_page", search_id, url, html} adds the post the browser shows to
// one, from its HTML as shown (job_scan/submit_page), and answers what
// JobFinder says. Each answers {error} when JobFinder says no, signed out
// included.

const api = globalThis.browser ?? globalThis.chrome;
const DEFAULT_BASE = "http://127.0.0.1:8765/";
// The browser stops an idle service worker after 30 seconds, a wait for
// JobFinder's answer included, and a model answering a form's questions can
// take minutes. Each extension call starts that time again, so one is made
// this often while the answer is awaited.
const KEEP_AWAKE_MS = 20000;

async function keptAwake(work) {
  const timer = setInterval(() => api.runtime.getPlatformInfo(), KEEP_AWAKE_MS);
  try {
    return await work;
  } finally {
    clearInterval(timer);
  }
}

async function base() {
  const { base } = await api.storage.local.get("base");
  return base || DEFAULT_BASE;
}

async function call(path, options = {}) {
  const url = new URL(path, await base());
  const desktop = url.hostname === "127.0.0.1";
  let response;
  try {
    response = await fetch(url, { credentials: "include", ...options });
  } catch {
    throw new Error(desktop ? "Start the JobFinder app on this computer first." : "JobFinder can't be reached.");
  }
  // The desktop app has no sign-in: it says why it refused.
  if (desktop && response.status === 403) {
    throw new Error(await response.text());
  }
  if (response.status === 401 || response.status === 403 || response.redirected) {
    throw new Error("Sign in to Recruiter in this browser first.");
  }
  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    throw new Error(data.error ?? `JobFinder answered ${response.status}.`);
  }
  return response;
}

async function answer(message) {
  if (message.type === "matches") {
    const path = message.shortlist ? "match/history?shortlist=true" : "match/history";
    const { matches, not_joblogged } = await (await call(path)).json();
    return { matches, not_joblogged: not_joblogged ?? [] };
  }
  if (message.type === "logged") {
    await call("match/joblog_status", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id: Number(message.id), status: message.status, logged: true }),
    });
    return true;
  }
  if (message.type === "application_form") {
    const response = await keptAwake(
      call("match/application_form", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ id: Number(message.id), fields: message.fields }),
      }),
    );
    return response.json();
  }
  if (message.type === "applied") {
    const response = await call("match/applied", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id: Number(message.id) }),
    });
    return response.json();
  }
  if (message.type === "searches") {
    return (await (await call("job_scan/search_names")).json()).searches ?? [];
  }
  if (message.type === "send_page") {
    const response = await call("job_scan/submit_page", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ search_id: message.search_id ?? null, url: message.url, html: message.html }),
    });
    return (await response.json()).said;
  }
  if (message.type === "joblog") {
    return withCvrAddress(await (await call(`match/joblog/${Number(message.id)}`)).json());
  }
  if (message.type === "pdf") {
    const response = await call("match/pdf", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id: Number(message.id), kind: message.kind }),
    });
    if (!response.headers.get("Content-Type")?.startsWith("application/pdf")) {
      throw new Error((await response.json().catch(() => ({}))).error ?? "JobFinder sent no PDF.");
    }
    const name = /filename\*=UTF-8''([^;]+)/.exec(response.headers.get("Content-Disposition") ?? "");
    const bytes = new Uint8Array(await response.arrayBuffer());
    let binary = "";
    for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    return { name: name ? decodeURIComponent(name[1]) : `${message.kind}.pdf`, base64: btoa(binary) };
  }
  throw new Error(`Unknown message ${message.type}.`);
}

// When the post names no postcode and town, the company's registered address
// in the CVR register (cvrapi.dk), as the joblog panel on JobFinder does
// (modules/cv_match/js/app.js): only for a company whose name shares most
// words with the post's, and a failed lookup changes nothing.
const CVRAPI = "https://cvrapi.dk/api";
// A slow register must not hold up the joblog: after this, it is shown without.
const CVR_TIMEOUT_MS = 4000;
const LEGAL_FORM = /\s+(?:a\/s|aps|i\/s|p\/s|k\/s|ivs|amba|a\.m\.b\.a\.)\.?$/i;

async function withCvrAddress(entry) {
  const fields = Object.fromEntries(entry.sections.flatMap((section) => section.fields).map((field) => [field.name, field]));
  const company = fields.name?.value ?? "";
  if (!fields.postalCodeAndCity || fields.postalCodeAndCity.value !== "" || company === "") return entry;
  let found;
  try {
    const response = await fetch(`${CVRAPI}?${new URLSearchParams({ search: company.replace(LEGAL_FORM, ""), country: "dk" })}`, { headers: { Accept: "application/json" }, signal: AbortSignal.timeout(CVR_TIMEOUT_MS) });
    found = await response.json();
  } catch {
    return entry;
  }
  if (found?.error || !found?.zipcode || !found?.city || !sameCompany(company, String(found.name ?? ""))) return entry;
  fields.postalCodeAndCity.value = `${found.zipcode} ${found.city}`;
  if (fields.companyAddress && fields.companyAddress.value === "" && found.address) fields.companyAddress.value = found.address;
  if (fields.companyCountry && fields.companyCountry.value === "") fields.companyCountry.value = "Danmark";
  if (fields.unknownPostalOrCity) fields.unknownPostalOrCity.value = false;
  return entry;
}

function sameCompany(wanted, name) {
  const words = (text) => text.toLowerCase().split(/[^\p{L}\d]+/u).filter((word) => word.length >= 2 && !["as", "aps", "is", "ps", "ks", "ivs", "amba"].includes(word));
  const want = words(wanted);
  const have = new Set(words(name));
  return want.length > 0 && want.filter((word) => have.has(word)).length / want.length >= 0.5;
}

// sendResponse rather than a returned promise: Chrome doesn't take one.
api.runtime.onMessage.addListener((message, sender, sendResponse) => {
  answer(message).then(
    (data) => sendResponse({ data }),
    (error) => sendResponse({ error: error.message }),
  );
  return true;
});
