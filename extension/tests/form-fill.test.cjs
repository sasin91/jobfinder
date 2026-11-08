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
  '  globalThis.testing = { fieldState, requireUnchanged, fillField }; return;'), context);
const { fieldState, requireUnchanged, fillField } = context.testing;

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

test('detached and newly disabled controls are left alone', () => {
  for (const change of [{ isConnected: false }, { disabled: true }, { readOnly: true }]) {
    const element = input();
    const target = field(element);
    Object.assign(element, change);
    assert.throws(() => fillField(target, 'AI answer'), /kept your answer/);
  }
});
