// Automations: the words for a rule's trigger, conditions and steps, so the
// list and the form say the same thing.

export const DAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];

/** Triggers that aren't panel events, in the "When" menu. */
export const LASTING = [
  ['tps', 'TPS is below…'],
  ['players', 'The player count stays…'],
];
export const TIMED = [
  ['every', 'Every few minutes'],
  ['daily', 'Every day at…'],
];

/** "When" menu value for a trigger: the event id, or the kind. */
export const whenValue = (t) => (t.kind === 'event' ? t.event : t.kind);

/** A fresh trigger for a "When" menu value. */
export function triggerFor(v) {
  if (v === 'tps') return { kind: 'tps', below: 15, minutes: 2 };
  if (v === 'players') return { kind: 'players', at_most: true, count: 0, minutes: 30 };
  if (v === 'every') return { kind: 'every', minutes: 60 };
  if (v === 'daily') return { kind: 'daily', at: '04:00' };
  return { kind: 'event', event: v };
}

/** Whether "only while the server is up" makes sense by default. */
export const upByDefault = (t) => !(t.kind === 'event' && ['stop', 'crash', 'sleep', 'problem'].includes(t.event));

/** Step menu: value, label. Power steps use their action as the value. */
export const STEPS = [
  ['command', 'Run a console command'],
  ['backup', 'Back up'],
  ['restart', 'Restart the server'],
  ['stop', 'Stop the server'],
  ['start', 'Start the server'],
  ['url', 'Call a URL'],
  ['wait', 'Wait'],
];
export const stepValue = (s) => (s.kind === 'power' ? s.action : s.kind);
export function stepFor(v) {
  if (v === 'command') return { kind: 'command', line: '' };
  if (v === 'backup') return { kind: 'backup', worlds_only: true };
  if (v === 'url') return { kind: 'url', url: '' };
  if (v === 'wait') return { kind: 'wait', seconds: 60 };
  return { kind: 'power', action: v };
}
export const heavy = (steps) => steps.some((s) => s.kind === 'power' || s.kind === 'backup');

const plural = (n, w) => `${n} ${w}${n === 1 ? '' : 's'}`;

export function whenText(t, events) {
  switch (t.kind) {
    case 'event': return `When ${(events.find((e) => e.id === t.event)?.label || t.event).replace(/^./, (c) => c.toLowerCase())}`;
    case 'tps': return `When TPS is below ${t.below} for ${t.minutes} min`;
    case 'players': return `When ${t.at_most ? 'at most' : 'at least'} ${plural(t.count, 'player')} ${t.count === 1 ? 'is' : 'are'} on for ${t.minutes} min`;
    case 'every': return `Every ${t.minutes >= 60 && t.minutes % 60 === 0 ? plural(t.minutes / 60, 'hour') : `${t.minutes} min`}`;
    case 'daily': return `Every day at ${t.at}`;
  }
  return '';
}

export function onlyText(o) {
  const parts = [];
  if (o.running) parts.push('the server is up');
  if (o.players_min != null && o.players_max != null) parts.push(`${o.players_min}–${o.players_max} players`);
  else if (o.players_min != null) parts.push(`at least ${plural(o.players_min, 'player')}`);
  else if (o.players_max != null) parts.push(`at most ${plural(o.players_max, 'player')}`);
  if (o.from && o.to) parts.push(`${o.from}–${o.to}`);
  if (o.days?.length && o.days.length < 7) parts.push(o.days.map((d) => DAYS[d]).join(', '));
  return parts.length ? `only if ${parts.join(', ')}` : '';
}

export function stepText(s) {
  switch (s.kind) {
    case 'command': return s.line;
    case 'backup': return s.worlds_only ? 'back up worlds' : 'back up everything';
    case 'power': return s.action;
    case 'url': return `call ${s.url.split('://')[1]?.split('/')[0] || s.url}`;
    case 'wait': return `wait ${s.seconds} s`;
  }
  return '';
}

/** One line for the list: when · only if · steps · cooldown. */
export function describe(r, events) {
  return [whenText(r.trigger, events), onlyText(r.only), r.steps.map(stepText).join(', then '), r.cooldown_minutes ? `at most every ${r.cooldown_minutes} min` : '']
    .filter(Boolean)
    .join(' · ');
}

/** "just now", "4 min ago", "2 h ago", "3 days ago". */
export function since(at) {
  const s = Date.now() / 1000 - at;
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return plural(Math.floor(s / 86400), 'day') + ' ago';
}
