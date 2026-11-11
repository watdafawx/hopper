<script>
  // Settings › Access & devices: other web pages that may use this panel
  // (a Hopper Server page on another panel), and whether they may also
  // manage who can get in. Both only change here, on the panel's own page.
  import { onMount } from 'svelte';
  import { AppWindow, Plus, Trash2 } from '@lucide/svelte';
  import { api, put } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let s = $state(null);
  let failed = $state(false);
  let busy = $state(false);
  let typed = $state('');
  let error = $state('');

  async function load() {
    try {
      s = await api('/settings/web-access');
      failed = false;
    } catch {
      failed = true;
    }
  }

  async function save(origins, manage, done) {
    busy = true;
    try {
      s = await put('/settings/web-access', { origins, manage });
      error = '';
      if (done) say(done);
      return true;
    } catch (e) {
      error = e.message;
      return false;
    } finally {
      busy = false;
    }
  }

  async function add() {
    const o = typed.trim();
    if (!o) return;
    if (await save([...s.origins, o], s.manage, null)) {
      typed = '';
      say('Added. That page can now use this panel once someone signs in there.');
    }
  }

  const remove = (o) => save(s.origins.filter((x) => x !== o), s.manage, `${o} can no longer use this panel.`);
  const setManage = (on) =>
    save(s.origins, on, on ? 'Those pages can now manage who can get in.' : 'Only this page can manage who can get in.').then((ok) => ok || load());

  onMount(load);
</script>

<div class="card">
  <h3><AppWindow size={14} />Other web pages that may use this panel</h3>
  {#if s}
    <p class="small subtle">A Hopper Server page on another panel can show and run this panel's servers once its address is here. It still has to sign in to this panel.</p>
    {#each s.origins as o (o)}
      <div class="row og">
        <span class="mono small ell grow" title={o}>{o}</span>
        <button class="btn sm icon ghost" title="Remove" aria-label="Remove {o}" disabled={busy} onclick={() => remove(o)}><Trash2 size={13} /></button>
      </div>
    {:else}
      <div class="small muted">None. Only this panel's own page can use it.</div>
    {/each}
    <div class="new">
      <div class="row">
        <div class="input grow"><input bind:value={typed} placeholder="http://fox:8790" spellcheck="false" autocomplete="off" aria-label="Page address" oninput={() => (error = '')} onkeydown={(e) => e.key === 'Enter' && add()} /></div>
        <button class="btn sm pri" disabled={busy || !typed.trim()} onclick={add}><Plus size={13} />Add</button>
      </div>
      {#if error}<div class="tiny err">{error}</div>{/if}
      <p class="tiny muted">The page's address exactly as the browser shows it: http or https, name and port, nothing after.</p>
    </div>
    <div class="row mg">
      <div class="grow">
        <div class="small">Let these pages manage who can get in</div>
        <div class="tiny muted">Users, devices, API tokens, two-factor and the certificate. Off: only this page can change them.</div>
      </div>
      <label class="switch" title="Let other web pages manage who can get in"><input type="checkbox" checked={s.manage} disabled={busy} onchange={(e) => setManage(e.target.checked)} /><span></span></label>
    </div>
  {:else if failed}
    <p class="small muted">Couldn't load this setting. Open this page again to retry.</p>
  {:else}
    <div class="skel"><i></i></div><div class="skel"><i></i></div>
  {/if}
</div>

<style>
  .card { display: flex; flex-direction: column; gap: 8px; }
  .card h3 { margin-bottom: 2px; }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .og { padding: 6px 0; border-bottom: 1px solid var(--line); }
  .new { border-top: 1px solid var(--line); padding-top: 10px; display: flex; flex-direction: column; gap: 8px; }
  .mg { border-top: 1px solid var(--line); padding-top: 10px; gap: 12px; }
  .err { color: var(--red); }
</style>
