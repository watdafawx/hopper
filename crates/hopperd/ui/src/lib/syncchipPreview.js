// `?syncchip=NAME` on the panel URL shows a sync chip in the server header without a paired
// instance, to check how it looks and folds. Names match desktop's `--dialog sync-chip --state`.
import { serverLook, restartLook } from './syncchip.js';

const act = (label, primary = false) => ({ label, primary, run() {} });
const S = (state, other, title, line, actions, progress) => ({ look: serverLook(state, other), title, line, actions, progress });

const ONE = {
  's-in-sync': () => [S({ kind: 'in-sync' }, 'Skyblock Crew Pack', 'In sync with Skyblock Crew Pack', 'The mods match.', [act('Check again')])],
  's-differ': () => [S({ kind: 'differ', mods: 3 }, 'Skyblock Crew Pack', '3 mods differ from Skyblock Crew Pack', "Sync makes the server's mods match this instance.", [act('Sync to server', true), act('Check again')])],
  's-comparing': () => [S({ kind: 'comparing' }, 'Skyblock Crew Pack', 'Comparing with Skyblock Crew Pack', 'This takes a moment.', [], 0.4)],
  's-pushing': () => [S({ kind: 'working', work: 'pushing' }, 'Skyblock Crew Pack', 'Pushing to Skyblock Crew Pack', 'Sending the changed mods.', [], 0.55)],
  's-offline': () => [S({ kind: 'offline' }, 'Skyblock Crew Pack', 'Skyblock Crew Pack is offline', "It can't be checked until it's back.", [act('Check again', true)])],
  's-failed': () => [S({ kind: 'failed' }, 'Skyblock Crew Pack', 'Sync failed', 'Nothing was changed.', [act('Try again', true)])],
};

const RESTART = () => ({ look: restartLook(true, 'server'), title: 'Changed while the server runs', line: 'Restart it to load the changes.', actions: [act('Restart', true), act('Hide')] });

export function previewChips(name) {
  if (!name) return [];
  if (ONE[name]) return ONE[name]();
  if (name === 'rs-changed') return [RESTART()];
  if (name === 'rs-with-sync') return [...ONE['s-differ'](), RESTART()];
  if (name === 'both-busy') return [...ONE['s-pushing'](), ...ONE['s-comparing']()];
  if (name === 'both-needs') return [...ONE['s-failed'](), ...ONE['s-differ']()];
  return [];
}
