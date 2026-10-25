// Uploads one file to the panel's incoming folder in 8 MB chunks (the
// same protocol as file uploads), for imports like server pack zips.
import { post } from './api.js';

const CHUNK = 8 * 1024 * 1024;

function put(id, offset, blob, onBytes) {
  return new Promise((resolve, reject) => {
    const x = new XMLHttpRequest();
    x.open('PUT', `/api/v1/incoming/uploads/${id}?offset=${offset}`);
    x.setRequestHeader('X-Hopperd', '1');
    x.setRequestHeader('Content-Type', 'application/octet-stream');
    x.upload.onprogress = (e) => onBytes(offset + e.loaded);
    x.onload = () => {
      let body = null;
      try { body = JSON.parse(x.responseText); } catch { /* none */ }
      if (x.status === 200) resolve(body);
      else if (x.status === 409 && body?.offset != null) resolve({ offset: body.offset, moved: true });
      else reject(new Error(body?.error || `HTTP ${x.status}`));
    };
    x.onerror = () => reject(new Error('Connection lost'));
    x.send(blob);
  });
}

/** Uploads `file`; `onProgress(0..1)`. Resolves to its name in the incoming folder. */
export async function uploadIncoming(file, onProgress = () => {}) {
  const { id, path } = await post('/incoming/uploads', { path: file.name, size: file.size });
  let offset = 0;
  for (;;) {
    const r = await put(id, offset, file.slice(offset, offset + CHUNK), (n) => onProgress(Math.min(1, n / Math.max(1, file.size))));
    offset = r.offset;
    if (r.done) break;
  }
  onProgress(1);
  return path || file.name;
}
