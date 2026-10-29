// The router's state (GET /router's `state`) as a chip: class, label, and a
// short note on what it means. Shared by Access › Server addresses and a
// server's Address card so both say the same thing.
export function routerChip(r) {
  const port = r?.status?.listening;
  switch (r?.state) {
    case 'running':
      return { cls: 'running', label: 'Running', note: `on port ${port}` };
    case 'starting':
      return { cls: 'starting', label: 'Starting', note: `on port ${r.config.port}` };
    case 'error':
      return { cls: 'crashed', label: "Can't start", note: r.status.error || '' };
    case 'off':
      return { cls: '', label: 'Off', note: 'turned off in Access & devices' };
    default:
      return { cls: '', label: 'Not running', note: 'starts once a server has a saved address' };
  }
}
