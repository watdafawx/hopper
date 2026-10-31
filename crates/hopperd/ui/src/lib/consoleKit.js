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

/** "Steve is online.", "Steve and Alex are online.", "Steve, Alex and 3
 * others are online.". Same words as desktop
 * (hopper-core `who_online`). */
export function whoOnline(names) {
  const n = names.length;
  if (n === 1) return `${names[0]} is online.`;
  if (n === 2) return `${names[0]} and ${names[1]} are online.`;
  if (n === 3) return `${names[0]}, ${names[1]} and ${names[2]} are online.`;
  return `${names[0]}, ${names[1]} and ${n - 2} others are online.`;
}

/** What to ask before `action`, or '' to go ahead at once: a kill always,
 * a stop or restart only while players are on or when the panel can't
 * tell who is. Nobody on means nothing to lose. Matches desktop
 * (hopper-core `PanelAction::confirm_text`). */
export function powerQuestion(server, action) {
  const name = server.name;
  if (action === 'kill') {
    return `Kill ${name}? The process ends at once: the world isn't saved, so anything since the last autosave is lost.`;
  }
  if (action !== 'stop' && action !== 'restart') return '';
  const players = Array.isArray(server.players) ? server.players.filter((p) => p && p.trim()) : null;
  if (!players) return `${LABEL[action]} ${name}? Hopper Server can't read its state right now, so it can't tell who is online.`;
  if (!players.length) return '';
  return `${LABEL[action]} ${name}? ${whoOnline(players)}`;
}

/**
 * Starts, stops, restarts or kills the server, asking first where it costs
 * something (see powerQuestion). True when it was sent.
 */
export async function askPower(server, action) {
  const name = server.name;
  const text = powerQuestion(server, action);
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
