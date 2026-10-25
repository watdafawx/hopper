<script>
  // A mod's side panel: what it is, its actions (update, ignore updates,
  // reset config, switch site, remove) and the same Versions and changelogs
  // view the dialog shows.
  import { X, Download, Lock, LockOpen, Settings2, Trash2, ExternalLink, Shuffle } from '@lucide/svelte';
  import ModVersions from './ModVersions.svelte';
  import { resetConfig, setLocked, removeMods, switchSource } from './lib/modops.js';

  let { server, mod, onClose, onChanged, onUpdate } = $props();
  let busy = $state('');

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const site = $derived(mod.source === 'modrinth' ? 'Modrinth' : 'CurseForge');
  const sideText = { client: 'Client only', server: 'Server only', both: 'Client + server' };
</script>

<aside class="peek card col">
  <div class="row top">
    {#if mod.icon}<img src={mod.icon} alt="" width="40" height="40" />{:else}<span class="ph"></span>{/if}
    <div class="grow"><div class="nm">{mod.name}</div><div class="tiny muted">{mod.author ? `by ${mod.author}` : mod.source || 'unknown source'}{mod.side ? ` · ${sideText[mod.side] || mod.side}` : ''}</div></div>
    <button class="btn sm ghost icon" title="Close" onclick={onClose}><X size={14} /></button>
  </div>
  <div class="row wrap">
    {#if mod.update && !mod.locked}<button class="btn sm pri" disabled={!!busy} onclick={() => onUpdate(mod)}><Download size={13} />Update to {mod.update.to}</button>{/if}
    {#if mod.source}<button class="btn sm" onclick={async () => (await setLocked(base, [mod], !mod.locked)) && onChanged(false)}>{#if mod.locked}<LockOpen size={13} />Allow updates{:else}<Lock size={13} />Ignore updates{/if}</button>{/if}
    {#if mod.source}<button class="btn sm ghost" onclick={() => resetConfig(base, mod)}><Settings2 size={13} />Reset config…</button>{/if}
    <span class="sp"></span>
    {#if mod.page}<a class="btn sm ghost icon" href={mod.page} target="_blank" rel="noreferrer" title="Open on {site}"><ExternalLink size={13} /></a>{/if}
    {#if mod.source}<button class="btn sm ghost icon" title="Switch to {mod.source === 'modrinth' ? 'CurseForge' : 'Modrinth'}" disabled={!!busy} onclick={async () => (await switchSource(base, mod, alive, (s) => (busy = s))) && onChanged(true)}><Shuffle size={13} /></button>{/if}
    <button class="btn sm ghost icon bad" title="Remove (kept in Backups)" onclick={async () => { if (await removeMods(base, [mod], alive)) { onChanged(true); onClose(); } }}><Trash2 size={13} /></button>
  </div>
  {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
  <div class="tiny muted mono ell" title={mod.file}>{mod.file}</div>
  <ModVersions {server} {mod} onChanged={onChanged} />
</aside>

<style>
  .peek { gap: 8px; width: 400px; flex: none; position: sticky; top: 0; max-height: calc(100vh - 170px); overflow: auto; }
  .top img, .ph { width: 40px; height: 40px; border-radius: 8px; background: var(--raise); flex: none; }
  .nm { font-weight: 600; font-size: 14px; }
  .grow { flex: 1; min-width: 0; }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .icon { padding: 3px 5px; }
  .bad { color: var(--red); }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; }
  @keyframes s { to { transform: rotate(360deg); } }
  @media (max-width: 1150px) { .peek { position: fixed; right: 12px; top: 64px; bottom: 12px; max-height: none; z-index: 40; box-shadow: 0 20px 60px #000c; } }
</style>
