import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('../web/theme.js', import.meta.url), 'utf8');
function environment(saved, dark = false, brokenStorage = false) {
  const dataset = {};
  const media = { matches: dark, addEventListener: (event, callback) => { assert.equal(event, 'change'); media.changed = callback; } };
  const writes = [];
  const context = vm.createContext({ document: { documentElement: { dataset } }, matchMedia: () => media, localStorage: { getItem: () => { if (brokenStorage) throw new Error('unavailable'); return saved; }, setItem: (...values) => { if (brokenStorage) throw new Error('unavailable'); writes.push(values); } } });
  vm.runInContext(source, context);
  return { dataset, media, writes, context };
}

const system = environment(null);
assert.deepEqual(system.dataset, { theme: 'light', themePreference: 'system' });
system.media.matches = true;
system.media.changed();
assert.equal(system.dataset.theme, 'dark');
system.context.setNewPhotoTheme('light');
system.media.changed();
assert.equal(system.dataset.theme, 'light');
assert.equal(system.writes.at(-1)[1], 'light');
system.context.setNewPhotoTheme('dark');
system.media.matches = false;
system.media.changed();
assert.equal(system.dataset.theme, 'dark');
system.context.setNewPhotoTheme('invalid');
assert.equal(system.dataset.themePreference, 'dark');
assert.equal(environment('invalid', true).dataset.themePreference, 'system');
assert.equal(environment('dark').dataset.theme, 'dark');
const fallback = environment('dark', true, true);
assert.equal(fallback.dataset.themePreference, 'system');
fallback.context.setNewPhotoTheme('light');
assert.equal(fallback.dataset.theme, 'light');
console.log('Theme checks passed: initial system, live media changes, explicit overrides, persistence, invalid/unavailable storage.');
