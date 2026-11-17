// The one editor for "Add rule" and "Edit": a form that is either a message
// (saved as a webhook) or a list of steps (saved as an automation). This is its
// logic: the blank forms, what each store is sent, and whether it can be saved.
import { stepFor, whenText } from './rules.js';
import { eventsPhrase, messageOutcome, stepsPhrase } from './rulesList.js';

/** Events a new message starts with (everything but the noisy player ones). */
export const DEFAULT_MESSAGE_EVENTS = ['start', 'stop', 'crash', 'backup', 'problem', 'tps', 'disk', 'ram', 'copy_failed'];

const copy = (x) => JSON.parse(JSON.stringify(x));
const newOnly = () => ({ running: true, players_min: null, players_max: null, from: null, to: null, days: [] });

/** A new rule. `then` is 'message' or 'steps'. */
export function blankForm(then = 'message') {
  return {
    then, id: null, name: '', enabled: true, servers: [],
    url: '', events: [...DEFAULT_MESSAGE_EVENTS],
    trigger: { kind: 'event', event: 'join' }, only: newOnly(), steps: [stepFor('command')], cooldown_minutes: 0,
  };
}

/** An existing message, ready to edit (it stays a message). */
export function formFromHook(h) {
  return { ...blankForm('message'), id: h.id, name: h.name, url: h.url, events: [...h.events], servers: [...h.servers], enabled: h.enabled };
}

/** An existing action, ready to edit (it stays steps). */
export function formFromRule(r) {
  return {
    ...blankForm('steps'), id: r.id, name: r.name, enabled: r.enabled, servers: [...r.servers],
    trigger: copy(r.trigger), only: { days: [], ...copy(r.only) }, steps: copy(r.steps), cooldown_minutes: r.cooldown_minutes,
  };
}

/** Switching "Then" to steps carries a single picked event over as the trigger;
 * everything else keeps its own fields (the other kind's are just hidden). */
export function switchThen(f, then, ruleEventIds = []) {
  if (f.then === then) return;
  if (then === 'steps' && f.events.length === 1 && ruleEventIds.includes(f.events[0])) f.trigger = { kind: 'event', event: f.events[0] };
  f.then = then;
}

/** What POST/PUT /webhooks takes. */
export const hookBody = (f) => ({ name: f.name, url: f.url, events: f.events, servers: f.servers, enabled: f.enabled });

/** What POST/PUT /rules takes: numbers as numbers, blanks as none. */
export function ruleBody(f) {
  const n = (v) => (v === '' || v == null ? null : Number(v));
  const t = { ...f.trigger };
  for (const k of ['below', 'minutes', 'count']) if (k in t) t[k] = Number(t[k]);
  const steps = f.steps.map((s) => (s.kind === 'wait' ? { ...s, seconds: Number(s.seconds) } : s));
  const only = { ...f.only, players_min: n(f.only.players_min), players_max: n(f.only.players_max), from: f.only.from || null, to: f.only.to || null };
  return { name: f.name, enabled: f.enabled, servers: f.servers, trigger: t, only, steps, cooldown_minutes: Number(f.cooldown_minutes) || 0 };
}

/** Rules that start, stop, restart or back up need a cooldown (the panel says how long). */
export const needsCooldown = (f, heavy, minCooldown) => f.then === 'steps' && heavy && (Number(f.cooldown_minutes) || 0) < minCooldown;

/** Can this form be saved? */
export function canSave(f, heavy, minCooldown) {
  if (f.then === 'message') return !!f.url.trim() && f.events.length > 0;
  return !!f.name.trim() && f.steps.length > 0 && !needsCooldown(f, heavy, minCooldown);
}

/** The sentence at the top of the editor, live as it is filled in. */
export function formSentence(f, hookEvents = [], ruleEvents = []) {
  if (f.then === 'message') {
    const kind = /discord(app)?\.com\/api\/webhooks/.test(f.url) ? 'discord' : f.url.trim() ? 'json' : 'discord';
    return { when: `When ${eventsPhrase(f.events, hookEvents)}`, then: messageOutcome(kind) };
  }
  return { when: whenText(f.trigger, ruleEvents), then: stepsPhrase(f.steps) };
}
