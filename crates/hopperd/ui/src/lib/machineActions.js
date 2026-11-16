// What the server list and Settings › Other machines both do to a machine,
// each asked the same way in both places.
import { put, post, del } from './api.js';
import { say } from './toast.svelte.js';
import { confirmIt } from './dialog.svelte.js';
import { removePanel } from './panels.svelte.js';

// Updating a node runs its own update; this panel only asks and follows
// it (design/multi-panel.md, A3).
export async function updateNode(n, version) {
  if (!(await confirmIt(`Update ${n.name} to hopperd ${version}?`, { body: 'It downloads the new Hopper Server from GitHub and restarts into it, as its own update does. Its servers keep running.', ok: 'Update' }))) return false;
  try {
    await post(`/nodes/${encodeURIComponent(n.id)}/update`, {}, { home: true });
    say(`Updating ${n.name}`);
    return true;
  } catch (e) {
    say(e.message, true);
    return false;
  }
}

export async function maintainNode(n, on) {
  if (on && !(await confirmIt(`Put ${n.name} under maintenance?`, { body: 'Its alerts go quiet and nothing new starts there from this panel. Servers that are running keep running.', ok: 'Pause node' }))) return false;
  try {
    await put(`/nodes/${encodeURIComponent(n.id)}/maintenance`, { on }, { home: true });
    say(on ? `${n.name} is under maintenance` : `${n.name} is out of maintenance`);
    return true;
  } catch (e) {
    say(e.message, true);
    return false;
  }
}

export async function removeNode(n) {
  const ok = await confirmIt(`Remove ${n.name}?`, {
    body: 'This panel forgets it and closes its connection. Its servers keep running there. To add it again, join it with a new command.',
    ok: 'Remove',
    danger: true,
  });
  if (!ok) return false;
  try {
    await del(`/nodes/${encodeURIComponent(n.id)}`, { home: true });
    say(`Removed ${n.name}.`);
    return true;
  } catch (e) {
    say(e.message, true);
    return false;
  }
}

export async function dropPanel(p) {
  if (!(await confirmIt(`Remove ${p.name} from this page?`, { body: 'Its servers leave this list and this browser signs out of it. Nothing changes on that panel.', ok: 'Remove' }))) return false;
  removePanel(p);
  return true;
}
