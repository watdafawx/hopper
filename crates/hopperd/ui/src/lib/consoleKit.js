// The server page's shared bits, as Hopper desktop has them: how a console
// line reads (server_control.rs / hopper-core logs.rs), stable line keys
// while the console trims its top (line_select.rs `shift`), the power
// questions (ServerControl::request) and the player actions with their
// kick / ban question (panel_tabs/players.rs).
import { post } from './api.js';
import { say } from './toast.svelte.js';
import { ask, confirmIt } from './dialog.svelte.js';

/**
 * How a console line reads: 'e' error, 'w' warning, 'f' a stack frame
 * (counted with the errors, as desktop's "Errors and warnings only"),
 * 'h' Hopper's own notes, 'j' joins and leaves, 'c' chat, '' plain.
 */
export function kind(line) {
  if (/\[Hopper\]/.test(line)) return 'h';
  if (/ERROR|FATAL|Exception/.test(line)) return 'e';
  if (/WARN/.test(line)) return 'w';
  if (/^\s+at |^\s*Caused by:|^\s*\.\.\. \d+ more/.test(line)) return 'f';
  if (/ (joined|left) the game/.test(line)) return 'j';
  if (/<[^>]+> /.test(line)) return 'c';
  return '';
}

/**
 * How far a re-read window of lines moved: the fewest lines `old` lost from
 * its top so that what is left starts `now`. `old.length` when they share
 * nothing (a new run). Keys then stay with their lines.
 */
export function shift(old, now) {
  if (!old.length) return 0;
  for (let k = 0; k < old.length; k++) {
    const rest = old.length - k;
    if (rest > now.length) continue;
    // Cheap checks first: the first and last kept lines.
    if (old[k] !== now[0] || old[old.length - 1] !== now[rest - 1]) continue;
    let same = true;
    for (let i = 1; i < rest - 1; i++) {
      if (old[k + i] !== now[i]) {
        same = false;
        break;
      }
    }
    if (same) return k;
  }
  return old.length;
}

const LABEL = { start: 'Start', stop: 'Stop', restart: 'Restart', kill: 'Kill' };
const DONE = { start: 'Starting', stop: 'Stopping', restart: 'Restarting', kill: 'Killed' };

/**
 * Starts, stops, restarts or kills the server, asking first where it costs
 * something, in desktop's words: a kill always, a stop or restart with who
 * is online. True when it was sent.
 */
export async function askPower(server, action) {
  const name = server.name;
  const online = server.players?.length || 0;
  let text = '';
  if (action === 'kill') {
    text = `Kill ${name}? The process ends at once: the world isn't saved, so anything since the last autosave is lost.`;
  } else if (action === 'stop' || action === 'restart') {
    text = online > 0
      ? `${LABEL[action]} ${name}? ${online} player${online === 1 ? ' is' : 's are'} online.`
      : `${LABEL[action]} ${name}? Nobody is online.`;
  }
  if (text && !(await confirmIt(`${LABEL[action]} the server?`, { body: text, ok: LABEL[action], danger: action === 'kill' }))) {
    return false;
  }
  try {
    await post(`/servers/${encodeURIComponent(server.id)}/power`, { action });
    say(`${DONE[action]} ${name}`);
    return true;
  } catch (e) {
    say(e.message, true);
    return false;
  }
}

/** A player action ('kick', 'ban', 'op', 'deop', 'whitelist_add',
 * 'whitelist_remove', 'pardon'). True when the server took it. */
export async function playerAct(server, action, who, reason = '') {
  try {
    await post(`/servers/${encodeURIComponent(server.id)}/players`, { action, name: who, reason });
    return true;
  } catch (e) {
    say(e.message, true);
    return false;
  }
}

/** Asks before a kick or a ban, with an optional reason for the player,
 * then does it. True when it was done. */
export async function askKickOrBan(server, action, who) {
  const verb = action === 'kick' ? 'Kick' : 'Ban';
  const tail = action === 'kick' ? 'to leave now' : 'from joining again';
  const reason = await ask(`${verb} ${who}`, '', {
    body: `This asks ${who} ${tail}. The reason is shown to them.`,
    ok: verb,
    placeholder: 'Reason (optional)',
    danger: action === 'ban',
  });
  if (reason === null) return false;
  return playerAct(server, action, who, reason.trim());
}

/** A player's head (mc-heads), `size` px. */
export const head = (name, size = 22) => `https://mc-heads.net/avatar/${encodeURIComponent(name)}/${size}`;
