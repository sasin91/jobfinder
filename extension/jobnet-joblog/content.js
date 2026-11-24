// On Jobnet: a small panel next to the "Opret joblog" form that fills it in
// from a saved match in JobFinder's CV checker (match/joblog/{id}, see
// app/src/matches/joblog.rs), PDFs included. It never presses Gem: the
// person checks the form and saves it. The matches applied for but not on
// Jobnet yet come first, and when the person saves a filled form with Gem,
// the match is marked as on Jobnet in JobFinder.
//
// Jobnet's form is React, which keeps its own copy of each value and ignores
// values set from outside. So a value is set through the element's own
// prototype setter and announced with an input event, as typing would; radio
// buttons and the checkbox are clicked; a field with suggestions (company,
// postcode and town) is typed into and its matching suggestion picked (the
// job title keeps typed text, so it is the match's own); the PDFs are attached to the upload fields with a DataTransfer.

(() => {
  const api = globalThis.browser ?? globalThis.chrome;

  // Jobnet's upload fields by the PDF that goes in them.
  const UPLOADS = { application: "file-Jobansoeninger", cv: "file-CV", resume: "file-CV", job: "file-Jobannoncer" };
  // Fields that list suggestions as you type.
  const TYPEAHEADS = ["name", "postalCodeAndCity"];
  // Jobnet's date fields, typed as dd-mm-yyyy.
  const DATES = ["submissionDeadline", "submissionDate"];
  const ROOT_ID = "jobfinder-joblog";
  const SYNC_ROOT_ID = "jobfinder-joblog-sync";
  // How long after Gem the form may take to go for it to count as saved.
  const SAVE_WINDOW_MS = 30000;
  // JobFinder status labels, and what Jobnet action each needs on a joblog card.
  const STATUS_LABELS = {
    IkkeSoegt: "Ikke søgt",
    Soegt: "Søgt",
    Samtale: "Samtale",
    Afslag: "Afslag (Afsluttet)",
    FaaetJob: "Fået job (Afsluttet)",
  };
  const LEGAL_FORMS = new Set(["as", "aps", "is", "ps", "ks", "ivs", "amba", "a/s", "i/s", "p/s", "k/s", "inc", "ltd", "llc", "gmbh", "danmark", "denmark"]);

  // The match the form was filled from ({id, label}); the same with the
  // status the form had, once Gem is pressed. When the form then goes,
  // Jobnet saved it, and the match is marked as on Jobnet in JobFinder
  // (afterForm). Closing it with Luk, Fortryd or Escape saves nothing.
  let filled = null;
  let saving = null;
  // How the last marking went, for the next panel to say: [text, ok].
  let marked = null;
  // State for the Joblog list status-sync panel.
  let syncDismissed = false;
  let syncLoading = false;
  let syncPending = null;
  let syncRefreshTimer = null;

  document.addEventListener(
    "submit",
    (event) => {
      if (filled === null || event.target !== form()) return;
      const status = form().querySelector('input[name="searchStatus"]:checked')?.value ?? "IkkeSoegt";
      saving = { ...filled, status, at: Date.now() };
    },
    true,
  );
  document.addEventListener(
    "click",
    (event) => {
      const button = event.target.closest?.("button");
      if (button && button.type !== "submit" && /^(Luk|Fortryd)$/.test(button.textContent.trim())) saving = null;
    },
    true,
  );
  document.addEventListener("cancel", () => (saving = null), true);

  // The form appears after Jobnet's page has loaded and goes when it leaves
  // it (one page app), so the panel follows it.
  new MutationObserver(watch).observe(document.documentElement, { childList: true, subtree: true });
  watch();

  function form() {
    return document.querySelector('dialog[open] input[name="occupation"]')?.closest("form")
      ?? document.querySelector('input[name="occupation"]')?.closest("form")
      ?? null;
  }

  function joblogCards() {
    const cards = [];
    for (const checkbox of document.querySelectorAll('input[type="checkbox"][data-testid^="input-joblogcard-afsluttet-"]')) {
      const uuid = checkbox.dataset.testid?.replace(/^input-joblogcard-afsluttet-/, "");
      const card = checkbox.closest("section.card") ?? checkbox.closest("li");
      if (!uuid || !card) continue;
      const heading = (card.querySelector("h3")?.textContent ?? "").replace(/\s+/g, " ").trim();
      const radios = {
        IkkeSoegt: card.querySelector(`input[data-testid="input-joblogcard-status-ikke-soegt-${CSS.escape(uuid)}"]`),
        Soegt: card.querySelector(`input[data-testid="input-joblogcard-status-soegt-${CSS.escape(uuid)}"]`),
        Samtale: card.querySelector(`input[data-testid="input-joblogcard-status-samtale-${CSS.escape(uuid)}"]`),
      };
      const status = Object.entries(radios).find(([, input]) => input?.checked)?.[0] ?? null;
      cards.push({ uuid, card, heading, radios, status, afsluttet: checkbox, closed: checkbox.checked });
    }
    return cards;
  }

  function watch() {
    const present = document.getElementById(ROOT_ID);
    const found = form();
    if (!found) {
      present?.remove();
      if (filled !== null) afterForm();
      watchSync();
      return;
    }
    document.getElementById(SYNC_ROOT_ID)?.remove();
    // "Opret joblog" is a modal: it sits in the top layer above any z-index and
    // makes the rest of the page inert. So the panel goes inside the modal and
    // into the top layer itself, as a popover opened after the modal.
    const target = found.closest('dialog[open], dialog, [role="dialog"], [aria-modal="true"]') ?? document.body;
    if (present?.parentNode === target) return;
    const host = present ?? panel();
    target.append(host);
    if (host.showPopover && !host.matches(":popover-open")) host.showPopover();
  }

  function watchSync() {
    const hasCards = Boolean(document.querySelector('input[type="checkbox"][data-testid^="input-joblogcard-afsluttet-"]'));
    const existing = document.getElementById(SYNC_ROOT_ID);
    if (!hasCards || syncDismissed) {
      existing?.remove();
      return;
    }
    if (existing) {
      // Re-match cards if Jobnet's infinite scroll rendered additional entries.
      clearTimeout(syncRefreshTimer);
      syncRefreshTimer = setTimeout(() => existing.__jobfinderRefreshCards?.(), 200);
      return;
    }
    if (syncLoading) return;
    syncLoading = true;
    ask({ type: "matches" })
      .then(({ needs_status_update }) => {
        syncLoading = false;
        syncPending = needs_status_update ?? [];
        if (syncDismissed || form() || !syncPending.length) return;
        if (!document.getElementById(SYNC_ROOT_ID)) {
          document.body.append(syncPanel(syncPending));
        }
      })
      .catch(() => {
        syncLoading = false;
      });
  }

  function afterForm() {
    const saved = saving;
    filled = null;
    saving = null;
    if (!saved || Date.now() - saved.at > SAVE_WINDOW_MS) return;
    ask({ type: "logged", id: saved.id, status: saved.status }).then(
      () => (marked = [`${saved.label}: marked as on Jobnet in JobFinder.`, true]),
      (error) => (marked = [`${saved.label}: saved on Jobnet, but JobFinder wasn't told (${error.message}). Tick "entered on Jobnet" there.`, false]),
    );
  }

  // ---------- the panel ----------

  function panel() {
    const host = document.createElement("div");
    host.id = ROOT_ID;
    host.popover = "manual";
    // A shadow root keeps Jobnet's styles out of the panel and the panel's out of Jobnet.
    const shadow = host.attachShadow({ mode: "open" });
    shadow.innerHTML = `
      <style>
        :host { all: initial; }
        .box { position: fixed; right: 16px; bottom: 16px; z-index: 2147483647; width: 320px; max-width: calc(100vw - 32px);
               font: 14px/1.4 system-ui, sans-serif; color: #1d1d1b; background: #fff; border: 1px solid #d6d6cf;
               border-radius: 12px; box-shadow: 0 8px 32px rgb(0 0 0 / 0.18); padding: 12px; }
        h2 { font-size: 15px; margin: 0 0 8px; display: flex; justify-content: space-between; }
        select, button { font: inherit; width: 100%; box-sizing: border-box; padding: 8px; border-radius: 8px; border: 1px solid #d6d6cf; background: #fff; color: inherit; }
        button { margin-top: 8px; cursor: pointer; }
        button.primary { background: #2f5bd3; border-color: #2f5bd3; color: #fff; font-weight: 600; }
        button:disabled { opacity: 0.6; cursor: wait; }
        .close { width: auto; margin: 0; padding: 0 6px; border: 0; background: none; font-size: 18px; line-height: 1; }
        ul { margin: 8px 0 0; padding-left: 18px; max-height: 40vh; overflow: auto; }
        li.miss { color: #a32c2c; }
        li.ok { color: #1f7a4d; }
        p { margin: 8px 0 0; color: #6b6b66; }
      </style>
      <div class="box" role="region" aria-label="JobFinder: Jobnet joblog">
        <h2>Fill from JobFinder <button class="close" type="button" aria-label="Close">×</button></h2>
        <select aria-label="Saved match"><option>Loading your saved matches…</option></select>
        <p class="waiting" hidden></p>
        <button class="primary fill" type="button" disabled>Fill in the form</button>
        <p class="note">Check the form afterwards, then press Gem yourself.</p>
        <ul class="log" hidden></ul>
      </div>`;
    const $ = (selector) => shadow.querySelector(selector);
    $(".close").addEventListener("click", () => host.remove());
    if (marked) {
      log($(".log"), ...marked);
      marked = null;
    }

    // The ids of the ones applied for but not on Jobnet yet (Søgt or
    // Samtale, entered nowhere), listed first.
    const waiting = new Set();

    ask({ type: "matches" })
      .then(({ matches, not_joblogged }) => {
        for (const m of not_joblogged) waiting.add(String(m.id));
        const others = matches.filter((m) => !waiting.has(String(m.id)));
        if (!not_joblogged.length && !others.length) throw new Error("No saved matches yet: match a job post in JobFinder's CV checker first.");
        if (!not_joblogged.length) {
          $("select").replaceChildren(...others.map(option));
        } else {
          $("select").replaceChildren(group("Applied, not on Jobnet yet", not_joblogged), ...(others.length ? [group("Other saved matches", others)] : []));
        }
        showWaiting();
        $(".fill").disabled = false;
      })
      .catch((error) => {
        $("select").replaceChildren(new Option("Couldn't load your matches"));
        log($(".log"), error.message, false);
      });

    function option(m) {
      const item = document.createElement("option");
      item.value = m.id;
      item.textContent = [m.job_title, m.company].filter(Boolean).join(" · ") || `Match ${m.id}`;
      return item;
    }

    function group(label, matches) {
      const list = document.createElement("optgroup");
      list.label = label;
      list.append(...matches.map(option));
      return list;
    }

    function showWaiting() {
      const count = waiting.size;
      $(".waiting").textContent = count === 1 ? "1 match applied for, not on Jobnet yet." : `${count} matches applied for, not on Jobnet yet.`;
      $(".waiting").hidden = count === 0;
    }

    $(".fill").addEventListener("click", async () => {
      const button = $(".fill");
      const id = $("select").value;
      button.disabled = true;
      $("select").disabled = true;
      button.textContent = "Filling in…";
      $(".log").replaceChildren();
      filled = null;
      try {
        await fill(Number(id), (text, ok) => log($(".log"), text, ok));
        filled = { id, label: $("select").selectedOptions[0]?.textContent ?? `Match ${id}` };
        log($(".log"), "Press Gem when it's right: JobFinder then marks it as on Jobnet.", true);
      } catch (error) {
        log($(".log"), error.message, false);
      }
      $("select").disabled = false;
      button.disabled = false;
      button.textContent = "Fill in the form";
    });

    return host;
  }

  function log(list, text, ok) {
    const item = document.createElement("li");
    item.className = ok ? "ok" : "miss";
    item.textContent = text;
    list.append(item);
    list.hidden = false;
  }

  // Firefox's browser.runtime.sendMessage returns a promise and takes no
  // callback; Chrome and Edge answer through the callback.
  async function ask(message) {
    const reply = globalThis.browser
      ? await globalThis.browser.runtime.sendMessage(message)
      : await new Promise((resolve, reject) => {
          api.runtime.sendMessage(message, (answer) => {
            if (api.runtime.lastError) reject(new Error(api.runtime.lastError.message));
            else resolve(answer);
          });
        });
    if (!reply || reply.error) throw new Error(reply?.error ?? "The extension didn't answer. Reload the page and try again.");
    return reply.data;
  }

  // ---------- filling ----------

  async function fill(id, report) {
    const entry = await ask({ type: "joblog", id });
    const fields = entry.sections.flatMap((section) => section.fields);
    // Deltid first: Antal timer pr. uge only shows after it.
    fields.sort((a, b) => (a.name === "jobType" ? -1 : b.name === "jobType" ? 1 : 0));
    for (const field of fields) {
      try {
        const done = await fillField(field);
        if (done) report(`${field.label}: ${done.length > 60 ? `${done.slice(0, 60)}…` : done}`, true);
      } catch (error) {
        report(`${field.label}: ${error.message}`, false);
      }
    }
    for (const file of entry.files) {
      if (!file.ready) {
        report(`${file.label}: ${file.missing}`, false);
        continue;
      }
      try {
        const pdf = await ask({ type: "pdf", id, kind: file.kind });
        attach(UPLOADS[file.kind], pdf);
        report(`${file.label}: ${pdf.name}`, true);
      } catch (error) {
        report(`${file.label}: ${error.message}`, false);
      }
    }
  }

  // Fills one field; returns what it did, or "" when there was nothing to fill.
  async function fillField(field) {
    const { name, type, value } = field;
    if (type === "radio") {
      const radio = form().querySelector(`input[type="radio"][name="${name}"][value="${CSS.escape(value)}"]`);
      if (!radio) throw new Error(`no "${field.options?.[value] ?? value}" option on the form`);
      if (!radio.checked) radio.click();
      return field.options?.[value] ?? value;
    }
    if (type === "checkbox") {
      const box = element(name);
      if (box.checked !== Boolean(value)) box.click();
      return value ? "ticked" : "";
    }
    if (value === "") return "";
    if (TYPEAHEADS.includes(name)) return pick(element(name), value);
    // The date applied only shows once Søgt or Samtale is picked.
    const node = (await until(() => form().querySelector(`[name="${name}"]`), 1500)) ?? element(name);
    setValue(node, value);
    if (DATES.includes(name)) {
      // The date picker reads the typed date; Escape closes its calendar.
      node.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      node.blur();
    }
    if (node.value !== String(value)) throw new Error(`Jobnet didn't take "${value}"`);
    return value;
  }

  function element(name) {
    const node = form().querySelector(`[name="${name}"]`);
    if (!node) throw new Error("not on the form (hidden, or Jobnet changed it)");
    return node;
  }

  function setValue(node, value) {
    const prototype = Object.getPrototypeOf(node);
    const setter = Object.getOwnPropertyDescriptor(prototype, "value")?.set;
    node.focus();
    if (setter) setter.call(node, value);
    else node.value = value;
    node.dispatchEvent(new Event("input", { bubbles: true }));
    node.dispatchEvent(new Event("change", { bubbles: true }));
  }

  // Jobnet only keeps a company or postcode picked from its suggestions (the
  // CVR register, postcodes): typed text is cleared when the field loses
  // focus. So the value is typed, then shorter searches made from it
  // ("Nordisk Fragt A/S" finds nothing, "Nordisk Fragt" finds "Nordisk Fragt
  // ApS"), and the suggestion sharing the most words with it is picked.
  async function pick(input, value) {
    const listbox = () => document.getElementById(input.getAttribute("aria-controls") ?? "");
    let best = null;
    for (const query of searches(input.name, value)) {
      setValue(input, query);
      const options = await until(() => {
        const found = [...(listbox()?.querySelectorAll('[role="option"]') ?? [])];
        return found.length ? found : null;
      }, 2500);
      for (const option of options ?? []) {
        const score = likeness(value, option.textContent);
        if (score > (best?.score ?? 0)) best = { option, score };
      }
      if (best?.score >= 1) break;
    }
    // Fewer than half the words in common is someone else's company or town.
    // A company Jobnet doesn't know keeps its name as typed.
    if (!best || best.score < 0.5) {
      setValue(input, input.name === "name" ? value : "");
      if (input.name === "name") return `${value} (as typed: none of Jobnet's suggestions fits)`;
      throw new Error(`none of Jobnet's suggestions fits "${value}": pick one yourself`);
    }
    // The option may have gone with a later search: search for it again.
    if (!best.option.isConnected) {
      setValue(input, best.option.textContent.trim());
      const again = await until(() => [...(listbox()?.querySelectorAll('[role="option"]') ?? [])].find((o) => o.textContent.trim() === best.option.textContent.trim()), 2500);
      if (!again) throw new Error(`Jobnet's suggestion "${best.option.textContent.trim()}" went away: pick one yourself`);
      best.option = again;
    }
    const picked = best.option.textContent.trim();
    for (const type of ["mousedown", "mouseup", "click"]) best.option.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true }));
    return picked.toLowerCase() === value.toLowerCase() ? picked : `${picked} (closest to "${value}")`;
  }

  // The searches to try for a value, the whole value first.
  function searches(name, value) {
    const list = [value];
    if (name === "postalCodeAndCity") list.push(value.slice(0, 4));
    if (name === "name") list.push(value.replace(/\s+(?:a\/s|aps|i\/s|p\/s|k\/s|ivs|amba|a\.m\.b\.a\.)\.?$/i, ""), words(value).slice(0, 2).join(" "));
    return [...new Set(list.filter((query) => query.length >= 2))];
  }

  function words(text) {
    return text.toLowerCase().split(/[^\p{L}\d]+/u).filter((word) => word.length >= 3);
  }

  // How alike a suggestion is to the value: 1 when it is the value, else the
  // share of the value's words found in it ("udvikler" in "softwareudvikler").
  function likeness(value, suggestion) {
    const text = suggestion.trim().toLowerCase();
    if (text === value.trim().toLowerCase()) return 1;
    const wanted = words(value);
    if (!wanted.length) return 0;
    return (wanted.filter((word) => text.includes(word)).length / wanted.length) * 0.99;
  }

  function until(check, ms) {
    return new Promise((resolve) => {
      const started = Date.now();
      const tick = () => {
        const found = check();
        if (found || Date.now() - started > ms) resolve(found);
        else setTimeout(tick, 100);
      };
      tick();
    });
  }

  function sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  function tokens(text) {
    return (text ?? "")
      .toLowerCase()
      .split(/[^\p{L}\d]+/u)
      .filter((word) => word.length >= 2 && !LEGAL_FORMS.has(word));
  }

  // Matches a JobFinder match against rendered Jobnet joblog cards ("<title> - <company>").
  // Uses saved Jobnet UUID first, otherwise requires strong company + title word overlap.
  function matchCard(match, cards) {
    if (match.jobnet_external_id) {
      const exact = cards.find((c) => c.uuid === match.jobnet_external_id);
      if (exact) return exact;
    }
    const titleWords = tokens(match.job_title);
    const companyWords = tokens(match.company);
    let best = null;
    for (const card of cards) {
      const heading = card.heading.toLowerCase();
      const companyHits = companyWords.filter((w) => heading.includes(w)).length;
      const titleHits = titleWords.filter((w) => heading.includes(w)).length;
      const companyRatio = companyWords.length ? companyHits / companyWords.length : 0;
      const titleRatio = titleWords.length ? titleHits / titleWords.length : 0;
      if (companyWords.length && companyRatio < 0.5) continue;
      const score = companyWords.length ? companyRatio * 0.6 + titleRatio * 0.4 : titleRatio;
      if (score >= 0.65 && score > (best?.score ?? 0)) {
        best = { card, score };
      }
    }
    return best?.card ?? null;
  }

  function statusLabel(status) {
    return STATUS_LABELS[status] ?? status ?? "Ukendt";
  }

  // Applies a JobFinder status to a live Jobnet card (radio + Afsluttet checkbox).
  // Jobnet's inline radio and Afsluttet controls save immediately via React onChange.
  async function applyCardStatus(card, targetStatus) {
    const wantClosed = targetStatus === "Afslag" || targetStatus === "FaaetJob";
    let wantRadio = null;
    if (targetStatus === "IkkeSoegt" || targetStatus === "Soegt" || targetStatus === "Samtale") {
      wantRadio = targetStatus;
    } else if (targetStatus === "Afslag") {
      // Keep Samtale if already interviewed; otherwise ensure Søgt.
      wantRadio = card.status === "Samtale" ? "Samtale" : "Soegt";
    } else if (targetStatus === "FaaetJob") {
      wantRadio = "Samtale";
    }
    if (wantRadio && card.status !== wantRadio) {
      const radio = card.radios[wantRadio];
      if (!radio) throw new Error(`Couldn't find "${wantRadio}" radio on this Jobnet card.`);
      radio.click();
      await sleep(300);
    }
    if (card.afsluttet && card.afsluttet.checked !== wantClosed) {
      card.afsluttet.click();
      await sleep(300);
    }
  }

  function syncPanel(initialPending) {
    const host = document.createElement("div");
    host.id = SYNC_ROOT_ID;
    const shadow = host.attachShadow({ mode: "open" });
    shadow.innerHTML = `
      <style>
        :host { all: initial; }
        .box { position: fixed; right: 16px; bottom: 16px; z-index: 2147483647; width: 360px; max-width: calc(100vw - 32px);
               font: 14px/1.4 system-ui, sans-serif; color: #1d1d1b; background: #fff; border: 1px solid #d6d6cf;
               border-radius: 12px; box-shadow: 0 8px 32px rgb(0 0 0 / 0.18); padding: 12px; }
        h2 { font-size: 15px; margin: 0 0 6px; display: flex; justify-content: space-between; align-items: center; gap: 8px; }
        .close { width: auto; margin: 0; padding: 0 6px; border: 0; background: none; font-size: 18px; line-height: 1; cursor: pointer; color: inherit; }
        .sub { margin: 0 0 8px; font-size: 12.5px; color: #6b6b66; }
        .items { list-style: none; margin: 0; padding: 0; max-height: 40vh; overflow: auto; display: flex; flex-direction: column; gap: 8px; }
        .item { border: 1px solid #e6e6e0; border-radius: 8px; padding: 8px; background: #fafaf7; }
        .item.done { border-color: #b7dfc8; background: #f2fbf5; }
        .title { font-weight: 600; font-size: 13px; margin: 0 0 2px; }
        .meta { font-size: 12px; color: #4d5b63; margin: 0 0 6px; }
        .row { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
        button { font: inherit; font-size: 12.5px; padding: 5px 10px; border-radius: 7px; border: 1px solid #d6d6cf; background: #fff; color: inherit; cursor: pointer; }
        button.primary { background: #2f5bd3; border-color: #2f5bd3; color: #fff; font-weight: 600; }
        button:disabled { opacity: 0.55; cursor: default; }
        .bar { display: flex; gap: 6px; margin-top: 10px; }
        .bar button { flex: 1; padding: 7px 10px; }
        .msg { font-size: 12px; margin: 6px 0 0; }
        .msg.ok { color: #1f7a4d; }
        .msg.miss { color: #a32c2c; }
      </style>
      <div class="box" role="region" aria-label="JobFinder: update Jobnet statuses">
        <h2><span class="heading">Update statuses on Jobnet</span> <button class="close" type="button" aria-label="Close">×</button></h2>
        <p class="sub">Status changes saved in JobFinder (e.g. from Gmail rejections or follow-ups) ready to apply on Jobnet.</p>
        <ul class="items"></ul>
        <div class="bar">
          <button class="primary update-all" type="button">Update all on screen</button>
          <button class="refresh" type="button">Refresh</button>
        </div>
        <p class="msg" hidden></p>
      </div>`;

    const $ = (selector) => shadow.querySelector(selector);
    let pending = [...initialPending];
    const doneIds = new Set();
    let busy = false;

    $(".close").addEventListener("click", () => {
      syncDismissed = true;
      host.remove();
    });

    function showMessage(text, ok = true) {
      const node = $(".msg");
      node.textContent = text;
      node.className = `msg ${ok ? "ok" : "miss"}`;
      node.hidden = !text;
    }

    async function syncOne(match, card) {
      await applyCardStatus(card, match.jobnet_status);
      await ask({
        type: "logged",
        id: match.id,
        status: match.jobnet_status,
        synced: true,
        external_id: card.uuid,
      });
      doneIds.add(match.id);
    }

    function render() {
      const cards = joblogCards();
      const remaining = pending.filter((m) => !doneIds.has(m.id));
      $(".heading").textContent = remaining.length
        ? `Update statuses on Jobnet (${remaining.length})`
        : "Jobnet statuses up to date";

      const list = $(".items");
      list.replaceChildren();
      let readyCount = 0;

      for (const match of pending) {
        const isDone = doneIds.has(match.id);
        const card = isDone ? null : matchCard(match, cards);
        if (card) readyCount++;

        const li = document.createElement("li");
        li.className = `item${isDone ? " done" : ""}`;

        const title = document.createElement("div");
        title.className = "title";
        title.textContent = [match.job_title, match.company].filter(Boolean).join(" · ") || `Match ${match.id}`;

        const meta = document.createElement("div");
        meta.className = "meta";
        const from = statusLabel(match.jobnet_logged_status);
        const to = statusLabel(match.jobnet_status);
        meta.textContent = isDone ? `Updated on Jobnet: ${to}` : `${from} → ${to}`;

        li.append(title, meta);

        if (!isDone) {
          const row = document.createElement("div");
          row.className = "row";
          if (card) {
            const updateBtn = document.createElement("button");
            updateBtn.type = "button";
            updateBtn.className = "primary";
            updateBtn.textContent = "Update";
            updateBtn.disabled = busy;
            updateBtn.addEventListener("click", async () => {
              busy = true;
              showMessage("");
              render();
              try {
                await syncOne(match, card);
                showMessage(`Updated ${match.company || match.job_title} on Jobnet.`, true);
              } catch (error) {
                showMessage(error.message, false);
              } finally {
                busy = false;
                render();
              }
            });

            const showBtn = document.createElement("button");
            showBtn.type = "button";
            showBtn.textContent = "Show card";
            showBtn.addEventListener("click", () => {
              card.card.scrollIntoView({ behavior: "smooth", block: "center" });
              card.card.style.outline = "3px solid #2f5bd3";
              card.card.style.outlineOffset = "3px";
              setTimeout(() => {
                card.card.style.outline = "";
                card.card.style.outlineOffset = "";
              }, 2500);
            });
            row.append(updateBtn, showBtn);
          } else {
            const hint = document.createElement("span");
            hint.className = "meta";
            hint.style.margin = "0";
            hint.textContent = "Scroll down or widen Jobnet's date range to load this card.";
            row.append(hint);
          }
          li.append(row);
        }
        list.append(li);
      }

      const updateAll = $(".update-all");
      updateAll.textContent = readyCount ? `Update all on screen (${readyCount})` : "No cards on screen yet";
      updateAll.disabled = busy || readyCount === 0;
      $(".refresh").disabled = busy;
    }

    $(".update-all").addEventListener("click", async () => {
      busy = true;
      showMessage("");
      render();
      let updated = 0;
      const errors = [];
      try {
        for (const match of pending) {
          if (doneIds.has(match.id)) continue;
          const card = matchCard(match, joblogCards());
          if (!card) continue;
          try {
            await syncOne(match, card);
            updated++;
            await sleep(250);
          } catch (error) {
            errors.push(`${match.company || match.job_title}: ${error.message}`);
          }
        }
        if (errors.length) {
          showMessage(`Updated ${updated}; failed: ${errors.join("; ")}`, false);
        } else {
          showMessage(`Updated ${updated} ${updated === 1 ? "entry" : "entries"} on Jobnet.`, true);
        }
      } finally {
        busy = false;
        render();
      }
    });

    $(".refresh").addEventListener("click", async () => {
      busy = true;
      showMessage("Checking JobFinder…", true);
      render();
      try {
        const { needs_status_update } = await ask({ type: "matches" });
        pending = needs_status_update ?? [];
        doneIds.clear();
        showMessage(pending.length ? "" : "Everything is synced with Jobnet.", true);
      } catch (error) {
        showMessage(error.message, false);
      } finally {
        busy = false;
        render();
      }
    });

    host.__jobfinderRefreshCards = () => {
      if (!busy) render();
    };
    render();
    return host;
  }

  function attach(name, pdf) {
    const input = document.querySelector(`input[type="file"][name="${name}"]`);
    if (!input) throw new Error("Jobnet's upload field isn't on the page");
    const bytes = Uint8Array.from(atob(pdf.base64), (c) => c.charCodeAt(0));
    const files = new DataTransfer();
    files.items.add(new File([bytes], pdf.name, { type: "application/pdf" }));
    input.files = files.files;
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  }
})();
