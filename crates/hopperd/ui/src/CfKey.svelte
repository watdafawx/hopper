<script>
  // The panel's CurseForge API key: set, change or remove it. In Access &
  // devices, and in the Mods tab while there is none.
  import { KeyRound, Save } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let { inline = false, onSaved = () => {} } = $props();
  let st = $state(null);
  let key = $state('');
  let saving = $state(false);
  let editing = $state(false);

  async function load() {
    try {
      st = await api('/settings/curseforge');
    } catch {
      st = null;
    }
  }

  async function save(clear = false) {
    saving = true;
    try {
      await api('/settings/curseforge', { method: 'PUT', body: { key: clear ? '' : key.trim() } });
      say(clear ? 'CurseForge key removed' : 'CurseForge key saved: CurseForge search and updates work now.');
      key = '';
      editing = false;
      await load();
      onSaved();
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  load();
</script>

<div class:card={!inline} class="col cf" class:inline>
  {#if !inline}<h3><KeyRound size={14} />CurseForge</h3>{/if}
  {#if st?.set && !editing}
    <div class="row small"><span class="chip running">Key set</span><span class="muted tiny">CurseForge search, identify and updates use it.</span><span class="sp"></span>
      <button class="btn sm" onclick={() => (editing = true)}>Change</button>
      {#if st.saved}<button class="btn sm ghost" disabled={saving} onclick={() => save(true)}>Remove</button>{/if}</div>
  {:else}
    <div class="small subtle">{inline ? 'CurseForge needs an API key for search, identify and updates. Modrinth works without one.' : 'An API key lets the panel search, identify and update CurseForge mods. Modrinth needs none.'}
      Get one at <a href="https://console.curseforge.com/" target="_blank" rel="noreferrer">console.curseforge.com</a>.</div>
    <form class="row" onsubmit={(e) => { e.preventDefault(); if (key.trim()) save(); }}>
      <div class="input grow"><input type="password" bind:value={key} placeholder="Paste your CurseForge API key" autocomplete="off" spellcheck="false" /></div>
      <button class="btn sm pri" disabled={!key.trim() || saving}><Save size={13} />{saving ? 'Checking…' : 'Save'}</button>
      {#if editing}<button type="button" class="btn sm ghost" onclick={() => (editing = false)}>Cancel</button>{/if}
    </form>
  {/if}
</div>

<style>
  .cf { gap: 8px; }
  .inline { background: var(--blue-dim); border: 1px solid #60a5fa44; border-radius: 8px; padding: 10px 12px; }
  .grow { flex: 1; padding: 5px 9px; }
</style>
