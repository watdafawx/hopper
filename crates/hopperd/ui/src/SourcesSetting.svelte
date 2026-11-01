<script>
  // Settings › Mod sources: mods from GitHub and other
  // links (off by default) and an optional GitHub token, as on desktop.
  import { Link, Save } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let st = $state(null);
  let token = $state('');
  let saving = $state(false);

  async function load() {
    try {
      st = await api('/settings/sources');
    } catch {
      st = null;
    }
  }

  async function put(body, msg) {
    saving = true;
    try {
      await api('/settings/sources', { method: 'PUT', body });
      if (msg) say(msg);
      token = '';
      await load();
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  load();
</script>

{#if st}
  <div class="card col src">
    <h3><Link size={14} />GitHub and other links</h3>
    <label class="row small"><input type="checkbox" checked={st.allowed} disabled={saving} onchange={(e) => put({ allowed: e.currentTarget.checked })} /><b>Allow mods from GitHub and other links</b></label>
    <div class="small subtle">Add content gets "From GitHub or a link", and a mod's menu gets "Follow a source". These files skip CurseForge's and Modrinth's checks, so add them only from people you trust.</div>
    {#if st.allowed}
      <div class="row small"><b>GitHub token</b><span class="chip {st.github_token ? 'running' : ''}">{st.github_token ? 'Saved' : 'None'}</span>
        {#if st.github_token}<span class="sp"></span><button class="btn sm ghost" disabled={saving} onclick={() => put({ github_token: '' }, 'GitHub token removed.')}>Remove</button>{/if}</div>
      <div class="small subtle">Optional. Without one GitHub allows 60 checks an hour; Hopper asks only when something may have changed. A token (no scopes needed) raises that to 5,000.</div>
      <form class="row" onsubmit={(e) => { e.preventDefault(); if (token.trim()) put({ github_token: token.trim() }, 'GitHub token saved.'); }}>
        <div class="input grow"><input type="password" bind:value={token} placeholder="Paste a GitHub token (optional)" autocomplete="off" spellcheck="false" /></div>
        <button class="btn sm" disabled={!token.trim() || saving}><Save size={13} />Save</button>
      </form>
    {/if}
  </div>
{/if}

<style>
  .src { gap: 8px; }
  .grow { flex: 1; padding: 5px 9px; }
</style>
