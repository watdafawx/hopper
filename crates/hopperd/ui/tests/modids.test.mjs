import test from 'node:test';
import assert from 'node:assert/strict';
import { readIds } from '../src/lib/modids.js';

const names = new Map([
  ['arcane_framework', 'Arcane Framework'],
  ['cloth-config', 'Cloth Config API'],
]);

test('an id in a sentence reads as the mod name, version and full stop kept', () => {
  assert.equal(readIds('Add what Spell Atlas needs: arcane_framework 1.0.', names), 'Add what Spell Atlas needs: Arcane Framework 1.0.');
  assert.equal(readIds('Add what X needs: arcane_framework.', names), 'Add what X needs: Arcane Framework.');
  assert.equal(readIds('needs arcane_framework, cloth-config', names), 'needs Arcane Framework, Cloth Config API');
});

test('an id inside a longer word, id, file or path stays', () => {
  assert.equal(readIds('arcane_framework_extras and my_arcane_framework', names), 'arcane_framework_extras and my_arcane_framework');
  assert.equal(readIds('see arcane_framework.mixins.json', names), 'see arcane_framework.mixins.json');
  assert.equal(readIds('config/cloth-config-client.toml', names), 'config/cloth-config-client.toml');
});

test('a name with $ is not read as a replacement pattern, and nothing known changes nothing', () => {
  assert.equal(readIds('needs a_b', new Map([['a_b', 'Cash $& Co']])), 'needs Cash $& Co');
  assert.equal(readIds('no ids here', names), 'no ids here');
  assert.equal(readIds('', names), '');
  assert.equal(readIds(undefined, names), '');
});

test('a dot in an id is a dot, not any character', () => {
  assert.equal(readIds('needs a.b and axb', new Map([['a.b', 'Dotted']])), 'needs Dotted and axb');
});
