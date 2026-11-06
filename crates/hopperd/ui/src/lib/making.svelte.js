// Servers being made (New server, Browse's Make a server), as desktop's
// server list shows them: a row at once with the job's progress and Cancel,
// then Ready with Start and Open, a grey Cancelled, or what went wrong. A
// row stays until it's opened, started or dismissed.
import { api, post } from './api.js';

export const making = $state([]);

const find = (job) => making.find((m) => m.job === job);

/** Lists job `job` making a server called `name` and follows it. */
export function track(job, name, start = false) {
  making.unshift({ job, name, start, state: 'running', detail: '', progress: null, id: null, error: '' });
  const poll = async () => {
    const m = find(job);
    if (!m) return;
    try {
      const st = await api(`/jobs/${job}`);
      m.detail = st.detail || '';
      m.progress = st.progress;
      if (!st.done) return setTimeout(poll, 600);
      if (st.cancelled || st.error === 'Cancelled') m.state = 'cancelled';
      else if (st.error) Object.assign(m, { state: 'failed', error: st.error });
      else {
        Object.assign(m, { state: 'ready', id: st.result?.id || null });
        if (m.start && m.id) post(`/servers/${encodeURIComponent(m.id)}/power`, { action: 'start' }).catch(() => {});
      }
    } catch (e) {
      Object.assign(m, { state: 'failed', error: e.message });
    }
  };
  poll();
}

/** Stops a server being made. */
export const cancel = (job) => post(`/jobs/${job}/cancel`, {}).catch(() => {});

/** Takes a row off the list. */
export function forget(job) {
  const i = making.findIndex((m) => m.job === job);
  if (i >= 0) making.splice(i, 1);
}
