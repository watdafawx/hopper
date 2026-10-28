<script>
  // The server's mod loader builds and what each changed, as Hopper
  // desktop's "Loader versions and changelog". "Use this version" installs
  // that build right away (the panel backs the server up first; Settings ›
  // Version has Undo).
  import { X, Check } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { followJob } from './lib/jobs.js';
  import { say } from './lib/toast.svelte.js';

  let { server, onClose, onChanged = () => {} } = $props();
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const NAMES = { neoforge: 'NeoForge', forge: 'Forge', fabric: 'Fabric', quilt: 'Quilt', paper: 'Paper', purpur: 'Purpur' };

  let info = $state(null);
  let err = $state('');
  let busy = $state('');

  const name = $derived(NAMES[info?.kind] || info?.kind || 'Loader');
  const log = $derived(info?.log || (info?.offered || []).map((version) => ({ version, text: '' })));
  const behind = $derived.by(() => {
    if (!info) return 0;
    const i = log.findIndex((c) => c.version === info.installed);
    return i < 0 ? 0 : i;
  });

  async function load() {
    try {
      info = await api(`${base}/version/loader-log`);
    } catch (e) {
      err = e.message;
    }
  }
  load();

  async function use(v) {
    busy = `Installing ${name} ${v}`;
    try {
      const { job } = await post(`${base}/version/change`, { minecraft: info.minecraft, loader_version: v });
      const r = await followJob(job, (st) => (busy = st.detail || busy));
      const ok = r.checks.every((c) => c.ok);
      say(ok ? `${server.name} is on ${name} ${v}. Undo is in Settings › Version.` : 'Changed, but a check failed: see Settings › Version.', !ok);
      onChanged();
      onClose();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
    }
  }
</script>

<div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onClose()}>
  <div class="box card col">
    <h3 class="row">Loader versions and changelog<span class="sp"></span><button class="btn sm ghost icon" title="Close" disabled={!!busy} onclick={onClose}><X size={14} /></button></h3>
    {#if err}<div class="err">{err}</div>
    {:else if !info}<div class="small muted row"><span class="spin"></span>Reading the changelog</div>
    {:else}
      <div class="tiny muted">Installed: {name} {info.installed || '(shows after the first start)'} · Minecraft {info.minecraft}</div>
      <div class="small subtle">{behind === 0 ? 'Up to date' : behind === 1 ? '1 build newer than yours' : `${behind} builds newer than yours, newest first`}</div>
      {#if info.error}<div class="tiny muted">The changelog couldn't be read: {info.error}</div>{/if}
      {#if alive}<div class="small gold">Stop the server to change its loader.</div>{/if}
      {#if busy}<div class="small gold row"><span class="spin"></span>{busy}…</div>{/if}
      <div class="scroll col">
        {#each log as c, i (c.version)}
          {@const installed = c.version === info.installed}
          {@const top = i === 0 && !installed}
          {@const usable = info.offered.includes(c.version)}
          <div class="card2" class:top class:installed>
            <div class="row">
              <b>{name} {c.version}</b>
              {#if installed}<span class="chip running xs"><Check size={11} />Installed</span>{/if}
              <span class="sp"></span>
              {#if !installed && usable}<button class="btn sm" class:pri={top} disabled={!!busy || alive} title={alive ? 'Stop the server first' : ''} onclick={() => use(c.version)}>Use this version</button>{/if}
            </div>
            <div class="small txt">{c.text || 'No notes for this build.'}</div>
          </div>
        {:else}<div class="small muted">No builds listed for Minecraft {info.minecraft}.</div>{/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .back { position: fixed; inset: 0; background: #0008; display: flex; justify-content: center; align-items: flex-start; padding: 56px 16px 16px; z-index: 60; }
  .box { width: min(680px, 100%); max-height: calc(100vh - 80px); gap: 10px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0; font-size: 15px; }
  .scroll { overflow: auto; min-height: 0; flex: 1; gap: 8px; padding-right: 4px; }
  .card2 { border: 1px solid var(--line); border-radius: 10px; padding: 10px 12px; display: flex; flex-direction: column; gap: 6px; }
  .card2.top { border-color: var(--acc); }
  .card2.installed { background: #ffffff08; }
  .txt { white-space: pre-wrap; color: var(--subtle); overflow-wrap: anywhere; }
  .chip.xs { font-size: 10.5px; padding: 0 6px; gap: 3px; }
  .gold { color: var(--gold); }
  .spin { width: 11px; height: 11px; border: 2px solid currentColor; border-right-color: transparent; border-radius: 99px; animation: rot .8s linear infinite; flex: none; }
  @keyframes rot { to { transform: rotate(360deg); } }
  .icon { padding: 3px 5px; }
</style>
