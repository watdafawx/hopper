import { post } from './api.js';
import { say } from './toast.svelte.js';

const DONE = { start: 'Starting', stop: 'Stopping', restart: 'Restarting', kill: 'Killed' };

/** Sends a power action and says what happened. */
export async function power(server, action) {
  if (action === 'kill' && !confirm(`Kill ${server.name}? The world is not saved.`)) return;
  try {
    await post(`/servers/${encodeURIComponent(server.id)}/power`, { action });
    say(`${DONE[action]} ${server.name}`);
  } catch (e) {
    say(e.message, true);
  }
}
