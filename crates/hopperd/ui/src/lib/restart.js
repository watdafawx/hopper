// Restarts the panel (the Windows service or systemd starts it again) and
// reloads once it answers. `onStatus` gets a line to show meanwhile.
import { post } from './api.js';
import { hello } from './boot.svelte.js';

export async function restartPanel(onStatus = () => {}) {
  onStatus('Restarting Hopper Server…');
  await post('/panel/restart').catch(() => {});
  await new Promise((r) => setTimeout(r, 1500));
  for (let i = 0; i < 60; i++) {
    try {
      await hello(3000);
      location.reload();
      return true;
    } catch {
      await new Promise((r) => setTimeout(r, 1000));
    }
  }
  onStatus("Hopper Server hasn't come back. If it isn't running as a service, start it again.");
  return false;
}
