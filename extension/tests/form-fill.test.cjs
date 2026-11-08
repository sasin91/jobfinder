const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

// Exercise the content script's actual functions without launching a browser or running
// its panel. Function declarations are hoisted; stop before the page-dependent entry point.
const source = fs.readFileSync(path.join(__dirname, '../jobnet-joblog/form-fill.js'), 'utf8');
const context = vm.createContext({ chrome: {}, Event });
vm.runInContext(source.replace('  const picked = window.__jobfinderForm;',
  '  globalThis.testing = { fieldState, requireUnchanged, fillField, readFields }; return;'), context);
const { fieldState, requireUnchanged, fillField, readFields } = context.testing;

function input(extra = {}) {
  return { value: '', checked: false, files: [], isConnected: true,
    disabled: false, readOnly: false, focus() {}, blur() {}, dispatchEvent() {}, ...extra };
}
function field(element, extra = {}) {
  const result = { kind: 'text', element, ...extra };
  result.initialState = fieldState(result);
  return result;
}

test('an untouched text field takes the answer', () => {
  const element = input();
  fillField(field(element), 'AI answer');
  assert.equal(element.value, 'AI answer');
});

test('typing while the model answers is preserved', () => {
  const element = input();
  const target = field(element);
  element.value = 'My answer';
  assert.throws(() => fillField(target, 'AI answer'), /kept your answer/);
  assert.equal(element.value, 'My answer');
});

test('a choice made while waiting is preserved', () => {
  const elements = [input(), input()];
  const target = field(elements[0], { kind: 'radio', elements, options: ['Yes', 'No'] });
  elements[1].checked = true;
  assert.throws(() => fillField(target, 'Yes'), /kept your answer/);
  assert.equal(elements[1].checked, true);
});

test('an upload selected during the PDF fetch is preserved', () => {
  const element = input();
  const target = field(element, { kind: 'file' });
  element.files = [{ name: 'my-cv.pdf', size: 42, lastModified: 1 }];
  assert.throws(() => requireUnchanged(target), /kept your answer/);
});

test('a file input reads the CV heading above the drop zone prompt', () => {
  const sibling = (textContent, extra = {}) => ({
    textContent, matches: () => false, querySelector: () => null, ...extra,
  });
  const body = {};
  const page = { body, defaultView: { getComputedStyle: () => ({ visibility: 'visible' }) } };
  const dropZone = {
    ownerDocument: page,
    getClientRects: () => [{}],
    parentElement: { parentElement: body },
    previousElementSibling: sibling('', { previousElementSibling: sibling('VEDHÆFTET CV') }),
  };
  const element = input({
    ownerDocument: page,
    tagName: 'INPUT',
    getAttribute: (name) => name === 'type' ? 'file' : null,
    closest: () => null,
    dataset: {},
    parentElement: dropZone,
    previousElementSibling: sibling('Tilføj filTræk fil hertil'),
  });
  page.querySelectorAll = (selector) => selector === 'iframe[title]' ? [] : [element];
  context.document = page;
  assert.equal(readFields().list[0].label, 'VEDHÆFTET CV');

  // Keep the generic prompt if the preceding upload has a heading of its own.
  dropZone.previousElementSibling = sibling('Cover letter', { querySelector: () => ({}) });
  assert.equal(readFields().list[0].label, 'Tilføj filTræk fil hertil');
});

test('nested upload inputs use the frame label and preserve uploaded attachments', () => {
  const form = {};
  const page = { defaultView: { getComputedStyle: () => ({ visibility: 'visible' }) } };
  const uploadPage = {};
  const element = input({ ownerDocument: uploadPage, dataset: {}, getAttribute: () => null });
  uploadPage.querySelectorAll = () => [element];
  const filenames = { textContent: '' };
  const frame = {
    ownerDocument: page, title: 'Application', contentDocument: uploadPage, isConnected: true,
    closest: () => form, getClientRects: () => [{}], parentElement: { querySelector: () => filenames },
  };
  page.querySelectorAll = (selector) => selector === 'iframe[title]' ? [frame] : [];

  const [upload] = readFields(page, form).list;
  assert.equal(upload.label, 'Application');
  assert.equal(upload.element, element);
  assert.doesNotThrow(() => requireUnchanged(upload));

  filenames.textContent = 'my-application.pdf';
  assert.throws(() => requireUnchanged(upload), /kept your answer/);
  assert.equal(readFields(page, form).kept, 1);
});

test('detached and newly disabled controls are left alone', () => {
  for (const change of [{ isConnected: false }, { disabled: true }, { readOnly: true }]) {
    const element = input();
    const target = field(element);
    Object.assign(element, change);
    assert.throws(() => fillField(target, 'AI answer'), /kept your answer/);
  }
});
