// Fills in an employer's own application form (Recruitee, Teamtailor,
// Greenhouse, Workday…) from a saved match in JobFinder, best effort. The
// toolbar popup puts it in the tab you have open (activeTab), with the match
// you picked in window.__jobfinderForm ({id, label}); it never runs on a page
// by itself.
//
// It reads the form's fields as shown (label, kind, options, whether required),
// skipping the ones that already have something in them, and asks JobFinder
// what goes in each (match/application_form, app/src/matches/application_form.rs):
// the profile's contact details, the match's PDFs for the uploads, the written
// application for a cover letter box, and the model's answers to the
// questions. It fills them in as typing and clicking would, outlines what it
// filled (green) and what it left for you (yellow), and lists both in a box in
// the corner. It never ticks a consent box and never sends the form: read
// every answer, then send it yourself.

(() => {
  const api = globalThis.browser ?? globalThis.chrome;
  const ROOT_ID = "jobfinder-form-fill";
  // Outlines on the page: filled in (pine) and left for you (marigold), as JobFinder's own colours.
  const FILLED_OUTLINE = "2px solid #2E6B5B";
  const LEFT_OUTLINE = "2px dashed #E2A33B";
  // How far up from a field to look for its question when no label names it.
  const ANCESTOR_STEPS = 6;
  // How many elements before a field (or its parents) may hold its question.
  const SIBLING_STEPS = 3;
  // Longest question text sent, in characters.
  const LONGEST_TEXT = 600;
  // Input types that are no question to answer.
  const NOT_FIELDS = ["hidden", "submit", "button", "reset", "image", "password", "search", "range", "color"];
  // The kinds JobFinder knows (application_form::kind), by input type.
  const INPUT_KINDS = { email: "email", tel: "tel", url: "url", number: "number", date: "date", text: "text", "": "text" };

  // The next field's key.
  let nextKey = 0;

  const picked = window.__jobfinderForm;
  if (!picked?.id) return;
  // The same page filled again: the box from last time goes first.
  document.getElementById(ROOT_ID)?.remove();
  run(picked);

  async function run(match) {
    const box = panel(match);
    let fields;
    try {
      fields = readFields();
    } catch (error) {
      box.line(`Couldn't read the form: ${error.message}`, "miss");
      box.done();
      return;
    }
    if (fields.list.length === 0) {
      box.line(fields.kept ? "Every field already has something in it: nothing to fill in." : "No form fields found on this page. If the form opens in a pop-up or a frame, open it on its own page first.", "miss");
      box.done();
      return;
    }
    box.status(`Asking JobFinder about ${fields.list.length} field${fields.list.length === 1 ? "" : "s"}… (the questions can take a minute)`);
    let answered;
    try {
      answered = await ask({ type: "application_form", id: match.id, fields: fields.list.map(({ element, elements, initialState, ...field }) => field) });
    } catch (error) {
      box.line(error.message, "miss");
      box.done();
      return;
    }
    const byKey = new Map(fields.list.map((field) => [field.key, field]));
    for (const answer of answered.answers) {
      const field = byKey.get(answer.key);
      if (!field) continue;
      try {
        if (answer.file) {
          box.status(`Getting the PDF for "${named(field)}"…`);
          const pdf = await ask({ type: "pdf", id: match.id, kind: answer.file });
          requireUnchanged(field);
          attach(field.element, pdf);
          mark(field, FILLED_OUTLINE);
          box.line(`${named(field)}: ${pdf.name} (${answer.from})`, "ok", field);
        } else {
          fillField(field, answer.value);
          mark(field, FILLED_OUTLINE);
          box.line(`${named(field)}: ${short(answer.value)} (${answer.from})`, "ok", field);
        }
      } catch (error) {
        mark(field, LEFT_OUTLINE);
        box.line(`${named(field)}: ${error.message}`, "miss", field);
      }
    }
    for (const left of answered.left) {
      const field = byKey.get(left.key);
      if (!field) continue;
      mark(field, LEFT_OUTLINE);
      box.line(`${named(field)}: ${left.why}`, "left", field);
    }
    if (fields.kept) box.line(`${fields.kept} field${fields.kept === 1 ? "" : "s"} already filled in: kept as they were.`, "note");
    box.done();
  }

  // ---------- reading the form ----------

  // The fields to fill: one per input, text box and drop-down, one per
  // group of radio buttons or checkboxes sharing a name. Each element gets
  // data-jobfinder-key, which the answers come back by.
  function readFields() {
    const list = [];
    const groups = new Map();
    let kept = 0;
    const controls = [...document.querySelectorAll("input, textarea, select")].filter((element) => !element.closest(`#${ROOT_ID}`));
    for (const element of controls) {
      const type = (element.getAttribute("type") ?? "").toLowerCase();
      if (element.tagName === "INPUT" && NOT_FIELDS.includes(type)) continue;
      if (element.disabled) continue;
      if (type === "file") {
        if (!shown(element.parentElement)) continue;
        if (element.files?.length) {
          kept++;
          continue;
        }
        list.push(field(element, "file", `${labelOf(element)} ${questionNear(element)}`));
        continue;
      }
      const choice = type === "radio" || type === "checkbox";
      // A styled radio button or checkbox hides its input and shows its label.
      const seen = choice ? shown(element) || shown(element.labels?.[0]) || shown(element.parentElement) : shown(element);
      if (!seen || element.readOnly) continue;
      if (choice && element.name) {
        const name = `${element.form ? [...document.forms].indexOf(element.form) : "-"}:${type}:${element.name}`;
        if (!groups.has(name)) groups.set(name, []);
        groups.get(name).push(element);
        continue;
      }
      if (type === "checkbox") {
        list.push(field(element, "checkbox", labelOf(element) || questionNear(element)));
        continue;
      }
      if (element.tagName === "SELECT") {
        if (element.multiple) continue;
        if (element.value !== "" && element.selectedIndex > 0) {
          kept++;
          continue;
        }
        const select = field(element, "select", labelOf(element) || questionNear(element));
        select.options = [...element.options].filter((option) => option.value !== "" && text(option)).map(text);
        list.push(select);
        continue;
      }
      if (element.value.trim() !== "") {
        kept++;
        continue;
      }
      const kind = element.tagName === "TEXTAREA" ? "textarea" : INPUT_KINDS[type];
      if (!kind) continue;
      list.push(field(element, kind, labelOf(element) || questionNear(element) || element.placeholder || ""));
    }
    for (const elements of groups.values()) {
      const first = elements[0];
      const type = first.type;
      if (elements.some((element) => element.checked)) {
        kept++;
        continue;
      }
      if (type === "checkbox" && elements.length === 1) {
        list.push(field(first, "checkbox", labelOf(first) || questionNear(first)));
        continue;
      }
      const group = field(first, type === "radio" ? "radio" : "checkboxes", groupQuestion(elements));
      group.elements = elements;
      group.options = elements.map((element) => labelOf(element) || element.value);
      group.required ||= elements.some((element) => element.required || element.getAttribute("aria-required") === "true");
      for (const element of elements) element.dataset.jobfinderKey = group.key;
      list.push(group);
    }
    for (const field of list) field.initialState = fieldState(field);
    return { list, kept };
  }

  function field(element, kind, label) {
    const key = `f${nextKey++}`;
    element.dataset.jobfinderKey = key;
    const label_ = cut(label.replace(/\s+/g, " ").trim());
    const hint = ["autocomplete", "name", "id", "accept", "placeholder"].map((name) => element.getAttribute(name) ?? "").join(" ").trim();
    const required = element.required || element.getAttribute("aria-required") === "true" || /\*\s*$/.test(label_);
    return { key, label: label_, kind, required, options: [], hint, element };
  }

  function shown(element) {
    return Boolean(element && element.getClientRects().length && getComputedStyle(element).visibility !== "hidden");
  }

  function text(node) {
    return (node?.textContent ?? "").replace(/\s+/g, " ").trim();
  }

  function cut(value) {
    return value.length > LONGEST_TEXT ? `${value.slice(0, LONGEST_TEXT)}…` : value;
  }

  // The field as the box names it: its label without the required star.
  function named(field) {
    return short(field.label.replace(/\s*\*\s*$/, "")) || "A field without a label";
  }

  function short(value) {
    const line = String(value).replace(/\s+/g, " ").trim();
    return line.length > 70 ? `${line.slice(0, 70)}…` : line;
  }

  // The element's own label: <label for>, a wrapping <label>, aria-labelledby or aria-label.
  function labelOf(element) {
    const parts = [...(element.labels ?? [])].map(text);
    const by = element.getAttribute("aria-labelledby");
    if (by) parts.push(by.split(/\s+/).map((id) => text(document.getElementById(id))).join(" "));
    parts.push(element.getAttribute("aria-label") ?? "");
    return parts.filter(Boolean).join(" ").trim();
  }

  // The text just before the field or one of its parents: a question shown
  // above the box without a <label>, or an upload's heading.
  function questionNear(element) {
    let node = element;
    for (let step = 0; step < ANCESTOR_STEPS && node && node !== document.body; step++) {
      let sibling = node.previousElementSibling;
      for (let count = 0; sibling && count < SIBLING_STEPS; count++, sibling = sibling.previousElementSibling) {
        if (sibling.querySelector?.("input, textarea, select")) break;
        const found = text(sibling);
        if (found) return found;
      }
      node = node.parentElement;
    }
    return "";
  }

  // A radio or checkbox group's question: its fieldset's legend, its
  // radiogroup's label, or the text above the smallest box holding them all.
  function groupQuestion(elements) {
    const legend = elements[0].closest("fieldset")?.querySelector("legend");
    if (legend && text(legend)) return text(legend);
    const group = elements[0].closest('[role="radiogroup"], [role="group"]');
    if (group && labelOf(group)) return labelOf(group);
    let holder = elements[0].parentElement;
    while (holder && !elements.every((element) => holder.contains(element))) holder = holder.parentElement;
    return holder ? questionNear(holder) : "";
  }

  // ---------- filling in ----------

  function fieldState(field) {
    return JSON.stringify((field.elements ?? [field.element]).map((element) => ({
      value: element.value,
      checked: element.checked,
      files: [...(element.files ?? [])].map((file) => [file.name, file.size, file.lastModified]),
    })));
  }

  function requireUnchanged(field) {
    const elements = field.elements ?? [field.element];
    if (elements.some((element) => !element.isConnected || element.disabled || element.readOnly)
        || fieldState(field) !== field.initialState) {
      throw new Error("changed while waiting: kept your answer");
    }
  }

  function fillField(field, value) {
    requireUnchanged(field);
    const element = field.element;
    if (field.kind === "radio" || field.kind === "checkboxes") {
      const wanted = field.kind === "radio" ? [value] : value.split("\n");
      for (const option of wanted) {
        const index = field.options.findIndex((shownOption) => shownOption === option);
        const choice = field.elements[index];
        if (!choice) throw new Error(`no "${option}" option on the form`);
        if (!choice.checked) choice.click();
      }
      return;
    }
    if (field.kind === "checkbox") {
      if (value === "yes" && !element.checked) element.click();
      return;
    }
    if (field.kind === "select") {
      const option = [...element.options].find((candidate) => text(candidate) === value);
      if (!option) throw new Error(`no "${value}" option on the form`);
      setValue(element, option.value);
      return;
    }
    setValue(element, value);
    if (element.value !== value) throw new Error("the page didn't take the answer: paste it in yourself");
  }

  // React and the like keep their own copy of each value and ignore values set
  // from outside: the value goes through the element's own prototype setter and
  // is announced with input and change events, as typing would.
  function setValue(element, value) {
    const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), "value")?.set;
    element.focus();
    if (setter) setter.call(element, value);
    else element.value = value;
    element.dispatchEvent(new Event("input", { bubbles: true }));
    element.dispatchEvent(new Event("change", { bubbles: true }));
    element.blur();
  }

  function attach(input, pdf) {
    const bytes = Uint8Array.from(atob(pdf.base64), (c) => c.charCodeAt(0));
    const files = new DataTransfer();
    files.items.add(new File([bytes], pdf.name, { type: "application/pdf" }));
    input.files = files.files;
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  }

  // The field's box on the page: a hidden upload's visible drop zone, a
  // group's box around its options.
  function shownPart(field) {
    if (field.elements) {
      let holder = field.elements[0].parentElement;
      while (holder && !field.elements.every((element) => holder.contains(element))) holder = holder.parentElement;
      return holder ?? field.element;
    }
    if (shown(field.element)) return field.element;
    return field.element.parentElement ?? field.element;
  }

  function mark(field, outline) {
    const part = shownPart(field);
    part.style.outline = outline;
    part.style.outlineOffset = "2px";
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

  // ---------- the box ----------

  function panel(match) {
    const host = document.createElement("div");
    host.id = ROOT_ID;
    // A shadow root keeps the page's styles out of the box and the box's out of the page.
    const shadow = host.attachShadow({ mode: "open" });
    shadow.innerHTML = `
      <style>
        :host { all: initial; }
        .box { position: fixed; right: 16px; bottom: 16px; z-index: 2147483647; width: 340px; max-width: calc(100vw - 32px);
               font: 14px/1.4 system-ui, sans-serif; color: #142430; background: #fff; border: 1px solid #cfd6d3;
               border-radius: 12px; box-shadow: 0 8px 32px rgb(0 0 0 / 0.18); padding: 12px; }
        h2 { font-size: 15px; margin: 0 0 4px; display: flex; justify-content: space-between; gap: 8px; }
        .close { font: inherit; border: 0; background: none; font-size: 18px; line-height: 1; cursor: pointer; padding: 0 4px; color: inherit; }
        .match { margin: 0 0 8px; color: #4d5b63; }
        .status { margin: 0; color: #4d5b63; }
        ul { margin: 8px 0 0; padding-left: 18px; max-height: 45vh; overflow: auto; }
        li { margin: 2px 0; }
        li.ok { color: #2E6B5B; }
        li.left { color: #8a5a00; }
        li.miss { color: #a32c2c; }
        li.note { color: #4d5b63; }
        li[data-key] { cursor: pointer; }
        .end { margin: 8px 0 0; font-weight: 600; }
        .applied { font: inherit; width: 100%; margin-top: 8px; padding: 8px; border-radius: 8px; cursor: pointer;
                   border: 1px solid #2E6B5B; background: #2E6B5B; color: #fff; font-weight: 600; }
        .applied:disabled { opacity: 0.6; cursor: default; }
        .marked { margin: 6px 0 0; color: #4d5b63; }
        [hidden] { display: none !important; }
      </style>
      <div class="box" role="region" aria-label="JobFinder: application form">
        <h2>Filled from JobFinder <button class="close" type="button" aria-label="Close">×</button></h2>
        <p class="match"></p>
        <p class="status" role="status">Reading the form…</p>
        <ul class="log" hidden></ul>
        <p class="end" hidden>Nothing was sent. Read every answer, then send the form yourself.</p>
        <button class="applied" type="button" hidden>I sent it: mark as applied</button>
        <p class="marked" role="status" hidden></p>
      </div>`;
    const $ = (selector) => shadow.querySelector(selector);
    $(".match").textContent = match.label;
    $(".close").addEventListener("click", () => host.remove());
    // Once the form is sent, the match is marked as applied for in JobFinder (Søgt), so it
    // shows under "Applied, not on Jobnet yet" for the joblog.
    $(".applied").addEventListener("click", async () => {
      $(".applied").disabled = true;
      $(".marked").hidden = false;
      $(".marked").textContent = "Telling JobFinder…";
      try {
        const applied = await ask({ type: "applied", id: match.id });
        $(".marked").textContent = `Marked as applied in JobFinder: ${applied.status}, ${applied.applied_on}.`;
      } catch (error) {
        $(".marked").textContent = error.message;
        $(".applied").disabled = false;
      }
    });
    document.body.append(host);
    return {
      status(said) {
        $(".status").textContent = said;
      },
      line(said, how, field) {
        const item = document.createElement("li");
        item.className = how;
        item.textContent = said;
        if (field) {
          item.dataset.key = field.key;
          item.title = "Show it on the page";
          item.addEventListener("click", () => shownPart(field).scrollIntoView({ behavior: "smooth", block: "center" }));
        }
        $(".log").append(item);
        $(".log").hidden = false;
      },
      done() {
        $(".status").hidden = true;
        $(".end").hidden = false;
        $(".applied").hidden = false;
      },
    };
  }
})();
