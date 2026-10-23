// The transfer queue: uploads go up in 8 MB chunks, three files at a time.
// A chunk that fails is retried; a paused or dropped upload asks the panel
// how far it got and carries on from there.

import { api, post, del } from './api.js';

const CHUNK = 8 * 1024 * 1024;
const PARALLEL = 3;
const RETRIES = 6;

export const transfers = $state({ list: [], open: false });

let nextId = 1;
const files = new Map(); // id -> File
const xhrs = new Map(); // id -> XMLHttpRequest
const listeners = new Set();

/** Called with (serverId, folder) when an upload finishes. */
export function onUploaded(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

function find(id) {
  return transfers.list.find((t) => t.id === id);
}

/** Speed over the last few seconds. */
function tick(t, bytes) {
  const now = performance.now();
  t.samples.push([now, bytes]);
  while (t.samples.length > 2 && now - t.samples[0][0] > 4000) t.samples.shift();
  const [t0, b0] = t.samples[0];
  t.speed = now > t0 ? ((bytes - b0) * 1000) / (now - t0) : 0;
  t.done = bytes;
}

/**
 * Queues uploads. `items` are [{file, path}] where path is relative to the
 * server folder. Existing files are only replaced with `overwrite`.
 */
export function queueUploads(server, items, overwrite = false) {
  for (const { file, path } of items) {
    const id = nextId++;
    files.set(id, file);
    transfers.list.push({
      id, kind: 'up', server, path, name: file.name, size: file.size, done: 0, speed: 0,
      state: 'queued', error: '', upload: null, overwrite, samples: [],
    });
  }
  transfers.open = true;
  pump();
}

function pump() {
  const running = transfers.list.filter((t) => t.state === 'running').length;
  let free = PARALLEL - running;
  for (const t of transfers.list) {
    if (free <= 0) break;
    if (t.state === 'queued') {
      t.state = 'running';
      free--;
      run(t.id);
    }
  }
}

function putChunk(t, offset, blob) {
  return new Promise((resolve, reject) => {
    const x = new XMLHttpRequest();
    xhrs.set(t.id, x);
    x.open('PUT', `/api/v1/fs/${encodeURIComponent(t.server)}/uploads/${t.upload}?offset=${offset}`);
    x.setRequestHeader('X-Hopperd', '1');
    x.setRequestHeader('Content-Type', 'application/octet-stream');
    x.upload.onprogress = (e) => {
      const tt = find(t.id);
      if (tt) tick(tt, offset + e.loaded);
    };
    x.onload = () => {
      xhrs.delete(t.id);
      let body = null;
      try { body = JSON.parse(x.responseText); } catch { /* none */ }
      if (x.status === 200) resolve(body);
      else if (x.status === 409 && body?.offset != null) resolve({ offset: body.offset, done: false, moved: true });
      else reject(Object.assign(new Error(body?.error || `HTTP ${x.status}`), { status: x.status }));
    };
    x.onerror = () => { xhrs.delete(t.id); reject(Object.assign(new Error('Connection lost'), { network: true })); };
    x.onabort = () => { xhrs.delete(t.id); reject(Object.assign(new Error('paused'), { aborted: true })); };
    x.send(blob);
  });
}

async function run(id) {
  const t = find(id);
  const file = files.get(id);
  try {
    let offset = 0;
    if (!t.upload) {
      const r = await post(`/fs/${encodeURIComponent(t.server)}/uploads`, { path: t.path, size: file.size, overwrite: t.overwrite });
      t.upload = r.id;
    } else {
      offset = (await api(`/fs/${encodeURIComponent(t.server)}/uploads/${t.upload}`)).offset;
    }
    let fails = 0;
    if (file.size === 0) {
      await putChunk(t, 0, new Blob([]));
    }
    while (offset < file.size) {
      if (find(id)?.state !== 'running') return;
      try {
        const r = await putChunk(t, offset, file.slice(offset, offset + CHUNK));
        offset = r.offset;
        tick(t, offset);
        fails = 0;
        if (r.done) break;
      } catch (e) {
        if (e.aborted) return;
        if (!e.network || ++fails > RETRIES) throw e;
        t.error = `Connection lost, retrying (${fails}/${RETRIES})…`;
        await new Promise((r) => setTimeout(r, 500 * 2 ** fails));
        offset = (await api(`/fs/${encodeURIComponent(t.server)}/uploads/${t.upload}`)).offset;
        t.error = '';
      }
    }
    t.state = 'done';
    t.done = file.size;
    t.speed = 0;
    files.delete(id);
    const folder = t.path.split('/').slice(0, -1).join('/');
    for (const fn of listeners) fn(t.server, folder);
  } catch (e) {
    t.state = 'failed';
    t.error = e.message;
  } finally {
    pump();
  }
}

export function pause(id) {
  const t = find(id);
  if (t?.state !== 'running' && t?.state !== 'queued') return;
  t.state = 'paused';
  t.speed = 0;
  xhrs.get(id)?.abort();
  pump();
}

export function resume(id) {
  const t = find(id);
  if (!t || !files.has(id)) return;
  t.state = 'queued';
  t.error = '';
  pump();
}

/** Retries a failed upload, replacing the file already there. */
export function replace(id) {
  const t = find(id);
  if (!t || !files.has(id)) return;
  t.overwrite = true;
  t.upload = null;
  resume(id);
}

export async function cancel(id) {
  const t = find(id);
  if (!t) return;
  const was = t.state;
  t.state = 'cancelled';
  xhrs.get(id)?.abort();
  files.delete(id);
  if (t.upload && was !== 'done') await del(`/fs/${encodeURIComponent(t.server)}/uploads/${t.upload}`).catch(() => {});
  transfers.list = transfers.list.filter((x) => x.id !== id);
  pump();
}

export function clearDone() {
  transfers.list = transfers.list.filter((t) => t.state !== 'done');
}

/** Starts a browser download (the panel supports ranges, so it resumes). */
export function download(url) {
  const a = document.createElement('a');
  a.href = url;
  a.download = '';
  document.body.appendChild(a);
  a.click();
  a.remove();
}
