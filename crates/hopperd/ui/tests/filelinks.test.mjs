import test from 'node:test';
import assert from 'node:assert/strict';
import { linkBody } from '../src/lib/filelinks.js';

const enc = encodeURIComponent;

test('a file, its preview and a zip of several become link requests', () => {
  assert.deepEqual(linkBody(`/api/v1/fs/srv1/file?path=${enc('config/a b.toml')}`), { server: 'srv1', body: { path: 'config/a b.toml', inline: false } });
  assert.deepEqual(linkBody(`/api/v1/fs/srv1/file?path=${enc('icon.png')}&inline=1`), { server: 'srv1', body: { path: 'icon.png', inline: true } });
  assert.deepEqual(linkBody(`/api/v1/fs/srv1/zip?path=world&path=${enc('world_nether')}`), { server: 'srv1', body: { paths: ['world', 'world_nether'], zip: true } });
  assert.deepEqual(linkBody('/api/v1/fs/srv1/zip?path=world&name=w.zip'), { server: 'srv1', body: { paths: ['world'], zip: true, name: 'w.zip' } });
});

test('an entry inside a zip and a backup', () => {
  assert.deepEqual(linkBody(`/api/v1/fs/srv1/zip-entry?path=${enc('mods/x.jar')}&entry=${enc('assets/x/icon.png')}&inline=1`), { server: 'srv1', body: { path: 'mods/x.jar', entry: 'assets/x/icon.png', inline: true } });
  assert.deepEqual(linkBody(`/api/v1/servers/srv1/backups/${enc('2026-11-13 12.00.hbk')}/download`), { server: 'srv1', body: { backup: '2026-11-13 12.00.hbk' } });
});

test('anything else has no link: it stays on its own page', () => {
  for (const u of ['/api/v1/fs/srv1/text?path=a', '/api/v1/servers/srv1/icon', '/api/v1/fs/srv1/file', 'https://evil.example/api/v1/fs/srv1/file?path=a', '/api/v1/servers/srv1/backups/a/b/download']) {
    assert.equal(linkBody(u), null, u);
  }
});
