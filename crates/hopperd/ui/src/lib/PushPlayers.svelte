<script>
  // "Push N to paired Hopper clients": the settings where players' copy
  // differs from the server's (untick to leave one out) and the paired
  // Hopper clients to send them to. Each client shows a notice and applies
  // them on one click; one that's offline gets it when it next connects.
  import { X, ArrowRight } from '@lucide/svelte';
  import { post, api } from './api.js';
  import { cfg, shortRel } from './configs.svelte.js';
  import { say } from './toast.svelte.js';
  import { ago } from './browse.js';

  let { server, onClose } = $props();

  const files = $derived((cfg.players?.files || []).filter((f) => f.diffs.length));
  const kid = (f, d) => `${f.rel}\u0000${(d.path || []).join('.')}\u0000${d.key}`;
  let picked = $state(new Set((cfg.players?.files || []).flatMap((f) => f.diffs.map((d) => kid(f, d)))));
  const devices = $derived(cfg.players?.devices || []);
  // Clients that sent a copy are ticked to start with.
  let to = $state(new Set((cfg.players?.devices || []).filter((d) => d.snapshot_at).map((d) => d.id)));
  let sending = $state(false);
  let sent = $state(null);

  const when = (s) => (s ? ago(new Date(s * 1000).toISOString()) : '');
  function flip(set, k) {
    const n = new Set(set);
    if (n.has(k)) n.delete(k);
    else n.add(k);
    return n;
  }

  async function send() {
    sending = true;
    try {
      let n = 0;
      for (const f of files) {
        const edits = f.diffs.filter((d) => picked.has(kid(f, d))).map((d) => ({ path: d.path, key: d.key, value: d.server }));
        if (!edits.length) continue;
        await post(`/servers/${encodeURIComponent(server.id)}/configs/push`, { devices: [...to], rel: f.client_rel, edits });
        n += edits.length;
      }
      say(`Sent ${n} setting${n === 1 ? '' : 's'} to ${to.size} Hopper client${to.size === 1 ? '' : 's'}. Each one shows a notice and applies them on one click.`);
      onClose();
    } catch (e) {
      say(e.message, true);
    } finally {
      sending = false;
    }
  }

  $effect(() => {
    api(`/servers/${encodeURIComponent(server.id)}/configs/pushes`).then((r) => (sent = r.pushes.slice(0, 8))).catch(() => {});
  });
  const STATE = { pending: 'Waiting', applied: 'Applied', dismissed: 'Turned down' };
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onClose()} />

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && onClose()}>
  <div class="box popover col" role="dialog" aria-label="Push settings to players">
    <h3 class="row">Push {picked.size} to paired Hopper clients<span class="sp"></span><button class="btn sm ghost icon" title="Close" onclick={onClose}><X size={14} /></button></h3>
    <div class="small subtle">The server's values go to each player's Hopper. Nothing changes on a player's PC until they click Apply there; their old copy goes to their backups.</div>
    <div class="rl">
      {#each files as f (f.rel)}
        <div class="rf mono">{shortRel(f.client_rel)}</div>
        {#each f.diffs as d (kid(f, d))}
          <label class="ri row-hover">
            <input type="checkbox" checked={picked.has(kid(f, d))} onchange={() => (picked = flip(picked, kid(f, d)))} />
            <span class="rn"><b>{d.label}</b><span class="mono tiny muted">{[...(d.path || []), d.key].join('.')}</span></span>
            <span class="ov mono"><s class="muted ell" title="Players' copy">{d.client}</s><ArrowRight size={12} /><span class="ell" title="The server's">{d.server}</span></span>
          </label>
        {/each}
      {/each}
    </div>
    <div class="lbl">Send to</div>
    <div class="devs">
      {#each devices as d (d.id)}
        <label class="ri dev row-hover">
          <input type="checkbox" checked={to.has(d.id)} onchange={() => (to = flip(to, d.id))} />
          <span class="rn"><b>{d.name}</b><span class="tiny muted">{d.snapshot_at ? `sent its copy ${when(d.snapshot_at)}` : 'no copy sent yet'}{d.last_seen ? ` · seen ${when(d.last_seen)}` : ''}</span></span>
        </label>
      {:else}
        <div class="small muted">No Hopper is paired with this panel yet.</div>
      {/each}
    </div>
    {#if sent?.length}
      <div class="lbl">Sent before</div>
      <div class="sent">
        {#each sent as p (p.id)}
          <div class="small row"><span class="ell">{p.device_name} · <span class="mono">{shortRel(p.rel)}</span> · {p.edits.length} setting{p.edits.length === 1 ? '' : 's'}</span><span class="sp"></span><span class="chip" class:warn={p.state === 'pending'} class:running={p.state === 'applied'}>{STATE[p.state] || p.state}</span></div>
        {/each}
      </div>
    {/if}
    <div class="row">
      <span class="sp"></span>
      <button class="btn ghost" onclick={onClose}>Cancel</button>
      <button class="btn pri" disabled={!picked.size || !to.size || sending} onclick={send}>{#if sending}<span class="spinner"></span>{/if}Push {picked.size} to {to.size} Hopper{to.size === 1 ? '' : 's'}</button>
    </div>
  </div>
</div>

<style>
  .box { width: min(640px, 100%); gap: 10px; padding: 18px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .rl { display: flex; flex-direction: column; overflow: auto; min-height: 0; max-height: 40vh; }
  .rf { font-size: 11.5px; color: var(--muted); padding: 10px 8px 4px; }
  .ri { display: grid; grid-template-columns: auto minmax(0, 1fr) minmax(0, 1.1fr); gap: 10px; align-items: center; padding: 6px 8px; border-radius: var(--radius); cursor: pointer; }
  .ri.dev { grid-template-columns: auto minmax(0, 1fr); }
  .ri input { accent-color: var(--acc); }
  .rn { display: flex; flex-direction: column; min-width: 0; }
  .ov { display: flex; align-items: center; gap: 6px; font-size: 12px; min-width: 0; justify-content: flex-end; }
  .ov :global(svg) { flex: none; color: var(--muted); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .devs, .sent { display: flex; flex-direction: column; gap: 2px; }
  .sent { gap: 6px; padding: 0 8px; }
  .icon { padding: 3px 5px; }
  @media (max-width: 700px) {
    .ri { grid-template-columns: auto minmax(0, 1fr); min-height: 44px; }
    .ov { grid-column: 2; justify-content: flex-start; }
    .btn { min-height: 44px; }
  }
</style>
