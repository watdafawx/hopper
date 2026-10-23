import { api } from './api.js';

/**
 * Follows a panel job until it ends. `onUpdate(state)` gets each poll.
 * Resolves with the job's result, rejects with its error.
 */
export function followJob(id, onUpdate = () => {}) {
  return new Promise((resolve, reject) => {
    const poll = async () => {
      try {
        const st = await api(`/jobs/${id}`);
        onUpdate(st);
        if (!st.done) {
          setTimeout(poll, 500);
        } else if (st.error) {
          reject(new Error(st.error));
        } else {
          resolve(st.result);
        }
      } catch (e) {
        reject(e);
      }
    };
    poll();
  });
}
