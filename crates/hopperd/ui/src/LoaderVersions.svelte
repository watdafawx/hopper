<script>
  // The server's mod loader builds and what each changed, as Hopper
  // desktop's "Loader versions and changelog". "Use this version" installs
  // that build right away (no backup: Settings › Version's Undo puts the
  // old start files back).
  import { X, Check } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { followJob } from './lib/jobs.js';
  import { say } from './lib/toast.svelte.js';
  import { dialog } from './lib/dialog.svelte.js';

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

<svelte:window onkeydown={(e) => e.key === 'Escape' && !e.defaultPrevented && !busy && !dialog.open && (e.preventDefault(), onClose())} />

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onClose()}>
  <div class="box popover col">
    <h3 class="row">Loader versions and changelog<span class="sp"></span><button class="btn sm ghost icon" title="Close" disabled={!!busy} onclick={onClose}><X size={14} /></button></h3>
    {#if err}<div class="err">{err}</div>
    {:else if !info}<div class="small muted row"><span class="spinner"></span>Reading the changelog</div>
    {:else}
      <div class="tiny muted">Installed: {name} {info.installed || '(shows after the first start)'} · Minecraft {info.minecraft}</div>
      <div class="small subtle">{behind === 0 ? 'Up to date' : behind === 1 ? '1 build newer than yours' : `${behind} builds newer than yours, newest first`}</div>
      {#if info.error}<div class="tiny muted">The changelog couldn't be read: {info.error}</div>{/if}
      {#if alive}<div class="small gold">Stop the server to change its loader.</div>{/if}
      {#if busy}<div class="small gold row"><span class="spinner"></span>{busy}…</div>{/if}
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
  .box { width: min(680px, 100%); max-height: calc(100dvh - var(--dialog-top) - 24px); gap: 10px; padding: 14px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .scroll { overflow: auto; min-height: 0; flex: 1; gap: 8px; padding-right: 4px; }
  .card2 { border: 1px solid var(--line); border-radius: 10px; padding: 10px 12px; display: flex; flex-direction: column; gap: 6px; }
  .card2.top { border-color: var(--acc); }
  .card2.installed { background: #ffffff08; }
  .txt { white-space: pre-wrap; color: var(--subtle); overflow-wrap: anywhere; }
  .chip.xs { font-size: 10.5px; padding: 0 6px; gap: 3px; }
  .gold { color: var(--gold); }
  .icon { padding: 3px 5px; }
</style>
