<script>
  // Settings › Access & devices: SFTP (FileZilla, WinSCP) for the servers'
  // files. Off on a new panel until the owner switches it on here; it works
  // at once, no restart. Switching off also ends open SFTP connections.
  import { onMount } from 'svelte';
  import { FolderTree } from '@lucide/svelte';
  import { api, put } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let s = $state(null);
  let failed = $state(false);
  let busy = $state(false);

  async function load() {
    try {
      s = await api('/settings/sftp');
      failed = false;
    } catch {
      failed = true;
    }
  }

  async function setOn(on) {
    busy = true;
    try {
      s = await put('/settings/sftp', { on });
      say(on ? `SFTP is on, port ${s.port}.` : 'SFTP is off. Open connections were closed.');
    } catch (e) {
      say(e.message, true);
      await load();
    } finally {
      busy = false;
    }
  }

  onMount(load);
</script>

<div class="card">
  <h3><FolderTree size={14} />SFTP</h3>
  {#if s}
    <div class="row mg">
      <div class="grow">
        <div class="small">Let programs like FileZilla and WinSCP open the servers' files</div>
        <div class="tiny muted">Off until you switch it on. Anyone who signs in this way has the same access as in the file manager.</div>
      </div>
      <label class="switch" title="SFTP"><input type="checkbox" checked={s.on} disabled={busy} onchange={(e) => setOn(e.currentTarget.checked)} /><span></span></label>
    </div>
    {#if s.on}
      <div class="fp"><div><div class="tiny muted">On port <b class="txt">{s.port}</b>: your name and password from the LAN or Tailscale, or an API token as the password from anywhere. Host key</div>
        <div class="mono tiny">{s.fingerprint}</div></div></div>
    {/if}
  {:else if failed}
    <p class="small muted">Couldn't load this setting. Open this page again to retry.</p>
  {:else}
    <div class="skel"><i></i></div>
  {/if}
</div>

<style>
  .card { display: flex; flex-direction: column; gap: 8px; }
  .card h3 { margin-bottom: 2px; }
  .grow { flex: 1; min-width: 0; }
  .mg { gap: 12px; }
  .txt { color: var(--text); }
  .fp { color: var(--blue); word-break: break-all; }
</style>
