import test from 'node:test';
import assert from 'node:assert/strict';
import { isShown, shownProject, isUnknown, shownBrowse } from '../src/lib/shown.js';

const override = { project_id: '0', source: '', shown_as: '282001', shown_source: 'curseforge', shown_mr: '' };

test('a pack override row shows as the pack mod but keeps no project', () => {
  assert.equal(isShown(override), true);
  assert.equal(shownProject(override), '282001');
  assert.equal(override.project_id, '0');
  assert.equal(isUnknown(override), false);
  assert.deepEqual(shownBrowse(override), { source: 'curseforge', id: '282001' });
});

test('a Modrinth entry links by its Modrinth id', () => {
  const m = { ...override, shown_as: '1234', shown_source: 'modrinth', shown_mr: 'AANobbMI' };
  assert.deepEqual(shownBrowse(m), { source: 'modrinth', id: 'AANobbMI' });
});

test('a row with a project of its own is not "shown"', () => {
  const m = { project_id: '55', source: 'curseforge', shown_as: '0' };
  assert.equal(isShown(m), false);
  assert.equal(shownProject(m), '55');
  assert.equal(shownBrowse(m), null);
});

test('a loose jar with nothing is still unknown', () => {
  const m = { project_id: '0', source: '' };
  assert.equal(isShown(m), false);
  assert.equal(shownProject(m), '0');
  assert.equal(isUnknown(m), true);
  // Rows from an older panel have no shown_* keys at all.
  assert.equal(isUnknown({ project_id: '0', source: '', shown_as: undefined }), true);
});

test('a followed link source is not unknown', () => {
  assert.equal(isUnknown({ project_id: '0', source: 'link' }), false);
});
