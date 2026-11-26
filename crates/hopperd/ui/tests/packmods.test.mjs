import test from 'node:test';
import assert from 'node:assert/strict';
import { packModsLabel, packUpdateNote } from '../src/lib/packmods.js';

// The same strings as desktop's addons::tests::pack_mods_label_names_a_newer_pack_only_when_there_is_one.
test('names a newer pack only when there is one', () => {
  assert.equal(packModsLabel(3, '8.6'), 'Pack mods can update separately (3) · pack v8.6 is out');
  assert.equal(packModsLabel(1, 'v2.9.3'), 'Pack mods can update separately (1) · pack v2.9.3 is out');
  assert.equal(packModsLabel(2, null), 'Pack mods can update separately (2)');
  assert.equal(packModsLabel(2, ''), 'Pack mods can update separately (2)');
  assert.equal(packModsLabel(2, '8.6', true), 'Pack mods can update (2)');
  assert.equal(packUpdateNote('1.2'), 'The pack ships 1.2. Players on the pack keep 1.2; the server will differ until the pack updates.');
});
