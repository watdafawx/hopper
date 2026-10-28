// Dates as Hopper desktop words them (hopper-core time::relative_secs and
// snapshots::when_label), for the Worlds, Backups and Schedule tabs.

const DAY = 86400;
const localDay = (secs) => {
  const d = new Date(secs * 1000);
  return Math.floor((d.getTime() / 1000 - d.getTimezoneOffset() * 60) / DAY);
};

/** "Today", "Yesterday", "3 days ago", "2 weeks ago", "1 month ago"… */
export function relative(secs) {
  const days = localDay(Date.now() / 1000) - localDay(secs);
  if (days <= 0) return 'Today';
  if (days === 1) return 'Yesterday';
  if (days <= 6) return `${days} days ago`;
  if (days <= 13) return '1 week ago';
  if (days <= 29) return `${Math.floor(days / 7)} weeks ago`;
  if (days <= 59) return '1 month ago';
  if (days <= 364) return `${Math.floor(days / 30)} months ago`;
  if (days <= 729) return '1 year ago';
  return `${Math.floor(days / 365)} years ago`;
}

/** "Today 14:05", "Yesterday 09:12", else as `relative`. */
export function whenLabel(secs) {
  const rel = relative(secs);
  if (rel !== 'Today' && rel !== 'Yesterday') return rel;
  const d = new Date(secs * 1000);
  return `${rel} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

/** The full date and time, for a tooltip. */
export const fullDate = (secs) => new Date(secs * 1000).toLocaleString();

/** "1 world", "3 worlds". */
export const plural = (n, what) => `${n} ${what}${n === 1 ? '' : 's'}`;
