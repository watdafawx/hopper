<script>
  // Settings › Panel folder: how long each server's .hopper-backups keeps
  // the old files behind Undo. Same three rules as Hopper desktop's
  // Settings › Library & folders › Backups.
  import { onMount } from 'svelte';
  import { Archive } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let r = $state(null);
  let gb = $state('');
  let dirty = $state(false);
  let failed = $state(false);

  const show = (v) => {
    r = { ...v };
    gb = String(Math.round((v.max_mb / 1024) * 100) / 100);
  };

  onMount(async () => {
    try {
      show(await api('/settings/backup-retention'));
    } catch {
      failed = true;
    }
  });

  const set = (k, v) => {
    r[k] = v;
    dirty = true;
  };

  async function save() {
    const body = { keep_actions: Math.max(0, Math.floor(Number(r.keep_actions) || 0)), keep_days: Math.max(0, Math.floor(Number(r.keep_days) || 0)), max_mb: Math.max(0, Math.round((Number(gb) || 0) * 1024)) };
    try {
      show(await api('/settings/backup-retention', { method: 'PUT', body }));
      dirty = false;
      say('Saved. They apply from the next backup; Clean up in Mods › Backups applies them now.');
    } catch (e) {
      say(e.message, true);
    }
  }
</script>

<div class="card">
  <h3><Archive size={14} />Backups<span class="sp"></span>{#if dirty}<button class="btn sm pri" onclick={save}>Save</button>{/if}</h3>
  {#if r}
    <p class="small subtle">Old files from updates, removals and syncs, kept in each server's .hopper-backups so you can undo. After each new backup the oldest past these rules go to the server's trash; backups from before wait for Clean up in Mods › Backups. World backups stay until you delete them.</p>
    <div class="f"><div class="l">Keep the last<small>Always kept, however old</small></div>
      <div class="input w"><input type="number" min="0" value={r.keep_actions} oninput={(e) => set('keep_actions', e.target.value)} /></div><span class="u muted small">actions</span></div>
    <div class="f"><div class="l">And anything newer than<small>Kept, however many</small></div>
      <div class="input w"><input type="number" min="0" value={r.keep_days} oninput={(e) => set('keep_days', e.target.value)} /></div><span class="u muted small">days</span></div>
    <div class="f"><div class="l">Up to<small>Per server. Past this the oldest go first; 0 is no limit</small></div>
      <div class="input w"><input type="number" min="0" step="0.5" value={gb} oninput={(e) => { gb = e.target.value; dirty = true; }} /></div><span class="u muted small">GB</span></div>
  {:else if failed}
    <p class="small muted">Couldn't load the backup rules. Open this page again to retry.</p>
  {:else}
    <div class="skel"><i></i></div><div class="skel"><i></i></div>
  {/if}
</div>

<style>
  .f { display: flex; align-items: center; gap: 10px; padding: 7px 0; border-bottom: 1px solid var(--line); }
  .f:last-child { border: 0; }
  .l { flex: 1; font-size: 12.5px; }
  .l small { display: block; color: var(--muted); font-size: 11px; }
  .input.w { width: 72px; }
  .u { width: 52px; }
</style>
