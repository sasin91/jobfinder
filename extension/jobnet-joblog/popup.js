// The toolbar button's popup: sends the job post in the tab you have open to
// JobFinder, which matches it like a post its scan found (job_scan/submit_page).
// It reads the page only when you open the popup (activeTab), as the browser
// shows it with its JavaScript run, so it works on LinkedIn and Indeed while
// you're signed in, without JobFinder asking those sites for anything.
//
// It also fills in the application form in that tab from a saved match:
// form-fill.js goes into the page with the match picked, and does the rest
// there, so the popup may close. The match whose post the page comes from (its
// address, or the company in the address or title) is picked to start with.
// "Mark as applied" marks that match as applied for in JobFinder.

const api = globalThis.browser ?? globalThis.chrome;
// The search picked last, picked again next time.
const LAST_SEARCH = "last_search_id";

const pageLine = document.getElementById("page");
const pick = document.getElementById("pick");
const searchList = document.getElementById("search");
const send = document.getElementById("send");
const said = document.getElementById("said");
const matchList = document.getElementById("match");
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

// The address without its query, as compared: host and path, no ending slash.
function place(url) {
  try {
    const parsed = new URL(url);
    return `${parsed.host}${parsed.pathname}`.replace(/\/+$/, "").toLowerCase();
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
  if (post && (page.startsWith(post) || post.startsWith(page))) score += SAME_POST;
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
  matchList.replaceChildren(
    ...all.map((match) => new Option([match.job_title, match.company].filter(Boolean).join(" · ") || `Match ${match.id}`, String(match.id))),
  );
  const best = all.reduce((best, match) => (likeness(match, tab) > likeness(best, tab) ? match : best), all[0]);
  matchList.value = String(best.id);
  fill.disabled = false;
  applied.disabled = false;
}

async function fillForm(tab) {
  fill.disabled = true;
  const id = Number(matchList.value);
  const label = matchList.selectedOptions[0]?.textContent ?? `Match ${id}`;
  try {
    await api.scripting.executeScript({
      target: { tabId: tab.id },
      func: (picked) => {
        window.__jobfinderForm = picked;
      },
      args: [{ id, label }],
    });
    await api.scripting.executeScript({ target: { tabId: tab.id }, files: ["form-fill.js"] });
    filled.textContent = "Filling in on the page: the box in its corner says what went in.";
  } catch (error) {
    filled.textContent = error.message;
  }
  fill.disabled = false;
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
  showMatches(tab).catch((error) => {
    matchList.replaceChildren(new Option("No matches to fill in from"));
    filled.textContent = error.message;
  });
  fill.addEventListener("click", () => fillForm(tab));
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
