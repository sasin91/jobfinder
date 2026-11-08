// The toolbar button's popup: sends the job post in the tab you have open to
// JobFinder, which matches it like a post its scan found (job_scan/submit_page).
// It reads the page only when you open the popup (activeTab), as the browser
// shows it with its JavaScript run, so it works on LinkedIn and Indeed while
// you're signed in, without JobFinder asking those sites for anything.
//
// It also fills in the application form in that tab from a saved match:
// form-fill.js goes into the page with the match picked, and does the rest
// there, so the popup may close. A match is picked when the page identifies it.
// "Mark as applied" marks that match as applied for in JobFinder.

const api = globalThis.browser ?? globalThis.chrome;
// The search picked last, picked again next time.
const LAST_SEARCH = "last_search_id";

const pageLine = document.getElementById("page");
const pick = document.getElementById("pick");
const searchList = document.getElementById("search");
const send = document.getElementById("send");
const said = document.getElementById("said");
const matchSearch = document.getElementById("match-search");
const matchActions = document.getElementById("match-actions");
const matchButtons = document.getElementById("match-buttons");
const matchList = document.getElementById("match");
const formPicker = document.getElementById("form-picker");
const formTarget = document.getElementById("form-target");
const fill = document.getElementById("fill");
const filled = document.getElementById("filled");
const applied = document.getElementById("applied");
// Words a company's name ends with that say nothing about which company it is.
const LEGAL_FORMS = ["as", "aps", "is", "ps", "ks", "ivs", "amba", "inc", "ltd", "llc", "gmbh", "ab", "oy"];
// How much each sign that the page belongs to a match counts.
const SAME_POST = 8;
const JOB_IN_TITLE = 4;
const COMPANY_IN_ADDRESS = 2;
const COMPANY_IN_TITLE = 1;

async function ask(message) {
  const answer = await api.runtime.sendMessage(message);
  if (answer?.error) throw new Error(answer.error);
  return answer?.data;
}

// The tab's address and its HTML as shown, read in the tab.
async function readTab(tab) {
  const [result] = await api.scripting.executeScript({
    target: { tabId: tab.id },
    func: () => ({ url: location.href, html: document.documentElement.outerHTML }),
  });
  return result.result;
}

async function showSearches() {
  const searches = await ask({ type: "searches" });
  if (searches.length === 0) {
    throw new Error("JobFinder has no job search to add the post to. Make one in JobFinder first (Settings, Job searches).");
  }
  const { [LAST_SEARCH]: last } = await api.storage.local.get(LAST_SEARCH);
  for (const search of searches) {
    const option = new Option(search.active ? search.name : `${search.name} (off)`, String(search.id));
    searchList.add(option);
  }
  if (searches.some((search) => String(search.id) === String(last))) searchList.value = String(last);
  pick.hidden = searches.length < 2;
}

function words(text) {
  return (text ?? "").toLowerCase().split(/[^\p{L}\d]+/u).filter((word) => word.length >= 3 && !LEGAL_FORMS.includes(word));
}

// Keep the query because some job boards use it to identify the posting.
function place(url) {
  try {
    const parsed = new URL(url);
    return `${parsed.origin}${parsed.pathname.replace(/\/+$/, "")}${parsed.search}`;
  } catch {
    return "";
  }
}

// How likely the page is the match's: its post's address, job title, and company.
function likeness(match, tab) {
  const page = place(tab.url);
  const post = place(match.job_url ?? "");
  const host = new URL(tab.url).host.toLowerCase();
  const title = (tab.title ?? "").toLowerCase();
  const job = (match.job_title ?? "").trim().toLowerCase();
  const company = words(match.company);
  let score = 0;
  if (post && page === post) score += SAME_POST;
  if (job && title.includes(job)) score += JOB_IN_TITLE;
  if (company.some((word) => host.includes(word))) score += COMPANY_IN_ADDRESS;
  if (company.some((word) => title.includes(word))) score += COMPANY_IN_TITLE;
  return score;
}

async function showMatches(tab) {
  const { matches, not_joblogged } = await ask({ type: "matches", shortlist: true });
  const seen = new Set();
  const all = [...matches, ...not_joblogged].filter((match) => !seen.has(match.id) && seen.add(match.id));
  if (all.length === 0) throw new Error("No matches available. Check Saved matches in JobFinder.");
  const options = all.map((match) => {
    const label = [match.job_title, match.company].filter(Boolean).join(" · ") || `Match ${match.id}`;
    return { option: new Option(label, String(match.id)), searchable: label.normalize().toLowerCase(), score: likeness(match, tab) };
  });
  const bestScore = options.reduce((best, { score }) => Math.max(best, score), 0);
  const likely = options.filter(({ score }) => score === bestScore && score > JOB_IN_TITLE);
  let preferred = likely.length === 1 ? likely[0].option.value : "";
  matchList.addEventListener("change", () => {
    preferred = matchList.value;
    matchButtons.disabled = !preferred;
  });

  function filterMatches() {
    const terms = matchSearch.value.normalize().toLowerCase().trim().split(/\s+/);
    const visible = options.filter(({ searchable }) => terms.every((term) => searchable.includes(term)));
    matchActions.disabled = visible.length === 0;
    if (matchActions.disabled) {
      matchList.replaceChildren(new Option("No matches found"));
      return;
    }
    const placeholder = new Option("Choose a match…", "");
    placeholder.disabled = true;
    matchList.replaceChildren(placeholder, ...visible.map(({ option }) => option));
    matchList.value = visible.some(({ option }) => option.value === preferred) ? preferred : "";
    matchButtons.disabled = !matchList.value;
  }

  matchSearch.addEventListener("input", filterMatches);
  matchSearch.disabled = false;
  filterMatches();
}

// Runs in the tab. Only metadata leaves the page during discovery.
function scanApplicationForms() {
  const confirmed = [];
  const formsByTarget = [];
  const external = [];
  function visible(element) {
    if (!element) return false;
    const { width, height } = element.getBoundingClientRect();
    return width > 0 && height > 0 && element.ownerDocument.defaultView.getComputedStyle(element).visibility === "visible";
  }

  function visit(doc, path) {
    const signals = new Map();
    for (const control of doc.querySelectorAll("input, textarea")) {
      if (control.disabled || control.readOnly || !visible(control.type === "file" ? control.parentElement : control)) continue;
      const form = control.form;
      if (!signals.has(form)) signals.set(form, { contact: new Set(), longAnswer: false, upload: false, applicationUpload: false });
      const signal = signals.get(form);
      const hints = [control.name, control.id, control.autocomplete, control.getAttribute("aria-label"),
        ...Array.from(control.labels ?? [], (label) => label.textContent)].join(" ");
      if (control.type === "file") {
        signal.upload = true;
        signal.applicationUpload ||= /\b(?:cv|resume|résumé|cover.?letter|curriculum)\b/i.test(hints.replaceAll("_", " "));
      } else if (control.tagName === "TEXTAREA") {
        signal.longAnswer = true;
      } else if (control.type === "email" || (control.type === "text" && /e.?mail/i.test(hints))) {
        signal.contact.add("email");
      } else if (control.type === "tel" || (control.type === "text" && /phone|telefon/i.test(hints))) {
        signal.contact.add("phone");
      } else if (control.type === "text" && /name|navn/i.test(hints)) {
        signal.contact.add("name");
      }
    }
    for (const [index, frame] of [...doc.querySelectorAll("iframe")].entries()) {
      if (!visible(frame)) continue;
      const src = URL.canParse(frame.src) ? new URL(frame.src) : null;
      if (src && /(?:^|\.)cookiebot\.com\.?$/i.test(src.hostname)) continue;
      if (frame.contentDocument) {
        const form = frame.closest("form");
        const label = (frame.title ?? "").trim();
        if (form && /^(?:cv|resume|résumé|application|cover letter)$/i.test(label)
          && frame.contentDocument.querySelector('input[type="file"]')) {
          if (!signals.has(form)) signals.set(form, { contact: new Set() });
          signals.get(form).applicationUpload = true;
          continue;
        }
        visit(frame.contentDocument, [...path, index]);
      } else if (src?.protocol === "https:") {
        external.push({ url: src.href, external: true });
      }
    }
    for (const [form, { contact, longAnswer, upload, applicationUpload }] of signals) {
      const hasDocuments = Boolean(applicationUpload || (upload && (contact.size > 0 || longAnswer)));
      if (hasDocuments || contact.size >= 2) {
        const targetIndex = formsByTarget.push({ doc, form }) - 1;
        confirmed.push({ path, url: doc.URL, targetIndex, hasDocuments,
          title: form?.getAttribute("aria-label") || doc.title });
      }
    }
  }
  visit(document, []);
  window.__jobfinderForms = formsByTarget;
  return { url: document.URL, confirmed, external };
}

async function applicationTargets(tab) {
  const [probe] = await api.scripting.executeScript({
    target: { tabId: tab.id }, func: scanApplicationForms,
  });
  const { confirmed, external } = probe.result;
  // A contact-only form on the page may be a signup next to the application frame.
  return confirmed.some(({ hasDocuments }) => hasDocuments) ? confirmed : [...confirmed, ...external];
}

function chooseApplicationTarget(tab, targets) {
  const options = targets.length ? targets : [{ path: [], url: tab.url }];
  if (options.length > 1) {
    formPicker.hidden = false;
    const placeholder = new Option("Choose a form…", "");
    placeholder.disabled = true;
    formTarget.replaceChildren(placeholder, ...options.map((option, i) => {
      const name = option.external ? new URL(option.url).host : option.title || (option.path.length ? "Embedded form" : "Main page");
      return new Option(`${i + 1}. ${name}`, String(i));
    }));
    formTarget.value = "";
    fill.disabled = true;
    formTarget.addEventListener("change", () => {
      fill.disabled = false;
    });
  } else if (options[0].external) {
    filled.textContent = `Embedded form on ${new URL(options[0].url).host}. Your browser may ask for access when you click Fill in.`;
  }
  fill.addEventListener("click", () => {
    const choice = options.length === 1 ? options[0] : options[formTarget.value];
    if (choice) fillForm(tab, choice);
  });
}

async function fillForm(tab, destination) {
  fill.disabled = true;
  formTarget.disabled = true;
  const id = Number(matchList.value);
  const label = matchList.selectedOptions[0]?.textContent ?? `Match ${id}`;
  try {
    const target = { tabId: tab.id };
    let form = destination;
    if (destination.external) {
      const { origin, hostname } = new URL(destination.url);
      // Request before the first await so the browser can tie it to the Fill in click.
      const granted = await api.permissions.request({
        permissions: ["webNavigation"],
        origins: [`https://${hostname}/*`],
      });
      if (!granted) throw new Error(`Allow access to ${origin} to fill in its embedded form.`);
      const frames = await api.webNavigation.getAllFrames({ tabId: tab.id });
      const candidates = frames?.filter(({ frameId, url }) => frameId !== 0 && url === destination.url);
      if (candidates?.length !== 1 || !frames.some(({ frameId, url }) => frameId === 0 && url === tab.url)) {
        throw new Error("The embedded form could not be identified. Reopen the extension when the form is visible.");
      }
      target.frameIds = [candidates[0].frameId];
      const [probe] = await api.scripting.executeScript({ target, func: scanApplicationForms });
      if (probe?.result?.url !== destination.url) throw new Error("The form changed. Open the extension again and retry.");
      const forms = probe.result.confirmed;
      if (forms?.length !== 1) {
        throw new Error(forms?.length ? `Several forms found on ${origin}. Open the application form on its own page.` : `No application fields found in the frame on ${origin}.`);
      }
      form = forms[0];
    }
    const [setup] = await api.scripting.executeScript({
      target,
      func: (picked, { path, url, targetIndex }, rootUrl) => {
        delete window.__jobfinderForm;
        if (document.URL !== rootUrl) return false;
        let formDocument = document;
        for (const index of path) {
          const frame = formDocument.querySelectorAll("iframe")[index];
          if (!frame?.getClientRects().length || !frame.contentDocument ||
            formDocument.defaultView.getComputedStyle(frame).visibility !== "visible") return false;
          formDocument = frame.contentDocument;
        }
        if (formDocument.URL !== url) return false;
        const scanned = window.__jobfinderForms?.[targetIndex];
        if (targetIndex !== undefined && (scanned?.doc !== formDocument || (scanned.form && !scanned.form.isConnected))) return false;
        const form = scanned?.form;
        window.__jobfinderForm = { ...picked, document: formDocument, form };
        return true;
      },
      args: [{ id, label }, form, destination.external ? destination.url : tab.url],
    });
    if (!setup?.result) throw new Error("The form changed. Open the extension again and retry.");
    await api.scripting.executeScript({ target, files: ["form-fill.js"] });
    filled.textContent = destination.external || destination.path.length
      ? "Filling in the embedded form. Look for the results at the top of the form."
      : "Filling in on the page: the box in its corner says what went in.";
  } catch (error) {
    filled.textContent = error.message;
  } finally {
    fill.disabled = false;
    formTarget.disabled = false;
  }
}

// Marks the picked match as applied for in JobFinder (Søgt), for an
// application sent some other way than a form it filled in.
async function markApplied() {
  applied.disabled = true;
  try {
    const marked = await ask({ type: "applied", id: Number(matchList.value) });
    filled.textContent = `Marked as applied in JobFinder: ${marked.status}, ${marked.applied_on}.`;
  } catch (error) {
    filled.textContent = error.message;
  }
  applied.disabled = false;
}

async function start() {
  const [tab] = await api.tabs.query({ active: true, currentWindow: true });
  if (!tab?.url || !/^https?:/.test(tab.url)) {
    pageLine.textContent = "Open a job post's page first, then press this button.";
    return;
  }
  pageLine.textContent = tab.title || tab.url;
  chooseApplicationTarget(tab, await applicationTargets(tab).catch(() => []));
  showMatches(tab).catch((error) => {
    matchList.replaceChildren(new Option("No matches to fill in from"));
    filled.textContent = error.message;
  });
  applied.addEventListener("click", markApplied);
  try {
    await showSearches();
  } catch (error) {
    said.textContent = error.message;
    return;
  }
  send.disabled = false;
  send.addEventListener("click", async () => {
    send.disabled = true;
    said.textContent = "Sending…";
    try {
      const page = await readTab(tab);
      const search_id = Number(searchList.value);
      await api.storage.local.set({ [LAST_SEARCH]: search_id });
      said.textContent = await ask({ type: "send_page", search_id, url: page.url, html: page.html });
    } catch (error) {
      said.textContent = error.message;
      send.disabled = false;
    }
  });
}

start();
