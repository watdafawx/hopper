<script>
  // A mod version with no changelog from its author (as in Hopper desktop's
  // files_changes.rs): with "Changes in the files" off, a notice offering to
  // compare the files; on, what the panel found in the jars.
  import { untrack } from 'svelte';
  import { api } from './api.js';
  import { say } from './toast.svelte.js';
  import Changelog from './Changelog.svelte';
  import { fc, isHidden, hide, NOTICE, LABEL, FOOTER, NONE, HELP } from './filesChanges.svelte.js';

  // `base` is the server's API path, `file` the mod's jar, `version` the file id
  // of this version; `enabled` what the panel said; `scan` false past a few per dialog;
  // `offer` false on all but the first card with no changelog (the notice shows once).
  let { base, file, project, version, enabled = false, scan = true, offer = true } = $props();
  const on = $derived(fc.on ?? enabled);
  let state = $state({ kind: 'idle', md: '', error: '' });
  let busy = $state(false);

  async function run() {
    state = { kind: 'running', md: '', error: '' };
    try {
      const r = await api(`${base}/mods/files-changes?file=${encodeURIComponent(file)}&version=${encodeURIComponent(version)}`);
      state = r.markdown ? { kind: 'done', md: r.markdown, error: '' } : { kind: 'none', md: '', error: '' };
    } catch (e) {
      state = { kind: 'failed', md: '', error: e.message };
    }
  }

  // Once per version, when the setting is on.
  let started = false;
  $effect(() => {
    if (on && scan && !started) {
      started = true;
      untrack(run);
    }
  });

  async function turnOn() {
    busy = true;
    try {
      await api('/settings/files-changes', { method: 'PUT', body: { on: true } });
      fc.on = true;
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }
</script>

{#if on}
  {#if !scan}<div class="empty muted"><i>{NONE}</i></div>
  {:else if state.kind === 'done'}
    <div class="col res">
      <div class="small muted">{LABEL}</div>
      <div class="notes"><Changelog markdown={state.md} /></div>
      <div class="tiny muted">{FOOTER}</div>
    </div>
  {:else if state.kind === 'none'}<div class="empty muted"><i>{NONE}</i></div>
  {:else if state.kind === 'failed'}
    <div class="col">
      <div class="empty muted"><i>{NONE}</i></div>
      <div class="tiny muted">Hopper couldn't compare the files. {state.error}</div>
    </div>
  {:else}
    <div class="small muted row"><span class="spinner blue"></span>Comparing the files</div>
  {/if}
{:else if !offer || isHidden(project)}
  <div class="empty muted"><i>{NONE}</i></div>
{:else}
  <div class="col notice">
    <div class="small muted">{NOTICE}</div>
    <div class="row">
      <button class="btn sm" disabled={busy} title={HELP} onclick={turnOn}>Turn on</button>
      <button class="btn sm ghost" disabled={busy} onclick={() => hide(project)}>Not now</button>
    </div>
  </div>
{/if}

<style>
  .col { display: flex; flex-direction: column; gap: 8px; }
  .empty { font-size: 13px; line-height: 1.6; }
  .spinner.blue { color: var(--blue); }
  .notes { background: var(--base); border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 10px; max-height: 320px; overflow: auto; overflow-wrap: anywhere; }
</style>
