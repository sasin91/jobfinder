const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../jobnet-joblog/popup.js'), 'utf8');
const topUrl = 'https://employer.example/jobs/1';
const externalUrl = 'https://applications.example/form?job=1';

function visible(ownerDocument) {
  return { ownerDocument, closest: () => null, getBoundingClientRect: () => ({ width: 500, height: 500 }), getClientRects: () => [{}] };
}

function page(url, groups = []) {
  const doc = { URL: url, title: 'Application', frames: [], forms: groups.map(() => ({ isConnected: true, getAttribute: () => null })),
    defaultView: { getComputedStyle: () => ({ visibility: 'visible' }) } };
  const controls = groups.flatMap((group, i) => group.map(([type, name]) => ({
    ...visible(doc), type, name, form: doc.forms[i], parentElement: visible(doc), getAttribute: () => null,
  })));
  doc.querySelectorAll = (selector) => selector === 'iframe' ? doc.frames : controls;
  doc.querySelector = (selector) => selector === 'input[type="file"]' ? controls.find(({ type }) => type === 'file') : null;
  return doc;
}

function popup({ top = page(topUrl), frame = page(externalUrl, [[['email', 'email'], ['text', 'fullName']]]), granted = true, matches = [] } = {}) {
  const elements = new Map();
  const calls = [];
  top.getElementById = (id) => {
    if (!elements.has(id)) elements.set(id, { value: '21', selectedOptions: [], listeners: {},
      addEventListener(event, listener) { this.listeners[event] = listener; },
      replaceChildren(...options) { this.options = options; },
    });
    return elements.get(id);
  };
  const context = vm.createContext({
    URL,
    Option: function (label, value) { this.text = label; this.value = value; },
    document: top,
    window: {},
    chrome: {
      runtime: { sendMessage: async () => ({ data: { matches, not_joblogged: [] } }) },
      permissions: { request: async (request) => { calls.push(request); return granted; } },
      webNavigation: { getAllFrames: async () => [
        { frameId: 0, url: topUrl },
        { frameId: 4, url: 'https://example.com/widget' },
        { frameId: 9, url: externalUrl },
      ] },
      scripting: { executeScript: async (injection) => {
        calls.push(injection);
        context.document = injection.target.frameIds ? frame : top;
        try {
          return [{ result: injection.func?.(...(injection.args ?? [])) }];
        } finally {
          context.document = top;
        }
      } },
    },
  });
  vm.runInContext(source.replace(/\bstart\(\);\s*$/, 'globalThis.testing = { applicationTargets, chooseApplicationTarget, fillForm, showMatches };'), context);
  return { ...context.testing, context, elements, calls };
}

const tab = { id: 1, url: topUrl };

test('visible same-origin forms take priority and autofill uses their document without extra access', async () => {
  const top = page(topUrl);
  const child = page('https://employer.example/apply', [[['email', 'newsletter']], [['email', 'email'], ['file', 'cv']]]);
  top.frames = [{ ...visible(top), src: externalUrl }, { ...visible(top), contentDocument: child }];
  const { applicationTargets, fillForm, context, calls } = popup({ top });
  const targets = await applicationTargets(tab);
  assert.equal(targets.length, 1);
  assert.equal(targets[0].url, child.URL);
  await fillForm(tab, targets[0]);
  assert.equal(context.window.__jobfinderForm.document, child);
  assert.equal(context.window.__jobfinderForm.form, child.forms[1]);
  assert.equal(calls[2].target.frameIds, undefined);
  assert.equal(calls[2].files[0], 'form-fill.js');
});

test('autofill skips Cookiebot and confirms the visible cross-origin application form', async () => {
  const top = page(topUrl);
  top.frames = [
    { ...visible(top), src: 'https://hidden.example/form', getBoundingClientRect: () => ({ width: 0, height: 0 }) },
    { ...visible(top), src: 'https://consentcdn.cookiebot.com/sdk/bc-v4.min.html' },
    { ...visible(top), src: externalUrl },
  ];
  const { applicationTargets, fillForm, calls } = popup({ top });
  const targets = await applicationTargets(tab);
  assert.equal(targets.length, 1);
  await fillForm(tab, targets[0]);

  assert.equal(calls[1].origins[0], 'https://applications.example/*');
  assert.equal(calls[2].target.frameIds[0], 9);
  assert.equal(calls[3].target, calls[2].target);
  assert.equal(calls[4].files[0], 'form-fill.js');
});

test('autofill finds the application inside a same-origin child of a cross-origin wrapper', async () => {
  const wrapper = page(externalUrl);
  const application = page('https://applications.example/pages/show-job/1', [[['text', 'fullName'], ['email', 'email']]]);
  wrapper.frames = [{ ...visible(wrapper), src: application.URL, contentDocument: application }];
  const { fillForm, calls, context } = popup({ frame: wrapper });

  await fillForm(tab, { external: true, url: externalUrl });

  assert.equal(context.window.__jobfinderForm.document, application);
  assert.equal(context.window.__jobfinderForm.form, application.forms[0]);
  assert.equal(calls[3].target.frameIds[0], 9);
  assert.equal(calls[3].files[0], 'form-fill.js');
});

test('an application tab with nested upload widgets is detected without contact fields', async () => {
  const wrapper = page(externalUrl);
  const application = page('https://applications.example/pages/show-job/1', [[['text', 'CV title']]]);
  wrapper.frames = [{ ...visible(wrapper), contentDocument: application }];
  application.frames = ['CV', 'Application'].map((title) => ({
    ...visible(application), title, closest: () => application.forms[0],
    contentDocument: page(`https://applications.example/upload/${title}`, [[['file', 'test']]]),
  }));
  const { fillForm, context } = popup({ frame: wrapper });
  await fillForm(tab, { external: true, url: externalUrl });
  assert.equal(context.window.__jobfinderForm.document, application);
  assert.equal(context.window.__jobfinderForm.form, application.forms[0]);
});

test('a cross-origin widget without application fields receives no match data', async () => {
  const { fillForm, calls, elements, context } = popup({ frame: page(externalUrl, [[['email', 'newsletter']]]) });
  await fillForm(tab, { external: true, url: externalUrl });
  assert.equal(calls.length, 2);
  assert.equal(context.window.__jobfinderForm, undefined);
  assert.match(elements.get('filled').textContent, /No application fields/);
});

test('declining frame access stops autofill', async () => {
  const { fillForm, calls, elements } = popup({ granted: false });
  await fillForm(tab, { external: true, url: externalUrl });
  assert.equal(calls.length, 1);
  assert.match(elements.get('filled').textContent, /Allow access/);
});

test('multiple plausible frames require a choice before asking for access', async () => {
  const { chooseApplicationTarget, calls, elements } = popup({ granted: false });
  chooseApplicationTarget(tab, [{ external: true, url: externalUrl }, { external: true, url: 'https://second.example/apply' }]);
  const picker = elements.get('form-target');
  const fill = elements.get('fill');
  assert.equal(fill.disabled, true);
  assert.match(picker.options[2].text, /second.example/);
  assert.equal(calls.length, 0);
  picker.value = '1';
  picker.listeners.change();
  assert.equal(fill.disabled, false);
  fill.listeners.click();
  assert.equal(calls[0].origins[0], 'https://second.example/*');
});

test('a contact form on the main page does not hide an external candidate', async () => {
  const top = page(topUrl, [[['email', 'email'], ['text', 'fullName']]]);
  top.frames = [{ ...visible(top), src: externalUrl }];
  const { applicationTargets } = popup({ top });
  const targets = await applicationTargets(tab);
  assert.equal(targets.length, 2);
  assert.equal(targets[0].url, topUrl);
  assert.equal(targets[1].url, externalUrl);
});

test('the main-page fallback runs without additional permissions', async () => {
  const { fillForm, calls } = popup();
  await fillForm(tab, { path: [], url: topUrl });
  assert.equal(calls.length, 2);
  assert.equal(calls[0].target.frameIds, undefined);
  assert.equal(calls[1].files[0], 'form-fill.js');
});

test('a replacement at the same URL cannot receive the selected form’s autofill', async () => {
  const top = page(topUrl, [[['email', 'email'], ['file', 'cv']]]);
  const { applicationTargets, fillForm, calls, context, elements } = popup({ top });
  const [target] = await applicationTargets(tab);
  top.forms[0].isConnected = false;
  top.forms[0] = { isConnected: true };

  await fillForm(tab, target);

  assert.equal(context.window.__jobfinderForm, undefined);
  assert.equal(calls.some(({ files }) => files?.includes('form-fill.js')), false);
  assert.match(elements.get('filled').textContent, /The form changed/);
});

test('the picker requires a choice for a similar URL and selects an exact match', async () => {
  const match = { id: 21, job_title: 'Backend Developer', company: 'Employer', job_url: topUrl };
  const similar = popup({ matches: [match] });
  similar.elements.get('match-search').value = '';
  await similar.showMatches({ ...tab, url: `${topUrl}2`, title: 'Employer' });
  assert.equal(similar.elements.get('match').value, '');
  assert.equal(similar.elements.get('match-buttons').disabled, true);

  await similar.showMatches({ ...tab, url: `${topUrl}?job=12`, title: 'Employer' });
  assert.equal(similar.elements.get('match').value, '');

  const exact = popup({ matches: [match] });
  exact.elements.get('match-search').value = '';
  await exact.showMatches(tab);
  assert.equal(exact.elements.get('match').value, '21');
  assert.equal(exact.elements.get('match-buttons').disabled, false);
});
