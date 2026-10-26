<script>
  // Sharing a log or crash report on mclo.gs. It becomes public, so this
  // says so, shows what gets hidden first, and only uploads on the click.
  // Opened from the Console, the Editor (logs and crash reports) and crash
  // banners: one dialog everywhere.
  import { Share2, X, Copy, ExternalLink, EyeOff } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { say } from './lib/toast.svelte.js';

  let { server, path = '', crash = false, onClose } = $props();
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  let logs = $state([]);
  let loaded = $state(false);
  let pick = $state('');
  let info = $state(null);
  let err = $state('');
  let busy = $state(false);
  let url = $state('');
  let showText = $state(false);

  const label = (f) => f.path.replace(/^logs\//, '').replace(/^crash-reports\//, 'crash report: ');
  const total = $derived(info ? info.hidden.ips + info.hidden.tokens + info.hidden.paths + info.hidden.users : 0);

  async function load() {
    try {
      logs = (await api(`${base}/logs`)).logs;
      loaded = true;
      if (!pick && logs.length) pick = path || ((crash && logs.find((f) => f.crash)) || logs[0]).path;
    } catch (e) {
      err = e.message;
    }
  }

  async function look() {
    info = null;
    err = '';
    showText = false;
    if (!pick) return;
    try {
      info = await api(`${base}/logs/preview?path=${encodeURIComponent(pick)}`);
    } catch (e) {
      err = e.message;
    }
  }

  async function share() {
    busy = true;
    err = '';
    try {
      url = (await post(`${base}/logs/share`, { path: pick })).url;
    } catch (e) {
      err = e.message;
    } finally {
      busy = false;
    }
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(url);
      say('Link copied');
    } catch {
      say("Couldn't copy; select the link instead.", true);
    }
  }

  function keys(e) {
    if (e.key === 'Escape' && !busy) onClose();
  }

  $effect(() => {
    base;
    load();
  });
  $effect(() => {
    pick;
    look();
  });
</script>

<svelte:window onkeydown={keys} />

<div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onClose()}>
  <div class="box card col">
    <div class="row"><h3><Share2 size={14} />{url ? 'Shared' : 'Share a log publicly?'}</h3><span class="sp"></span>
      <button class="btn sm ghost icon" title="Close" disabled={busy} onclick={onClose}><X size={14} /></button></div>
    {#if url}
      <div class="row link">
        <span class="chip running">Public</span>
        <a class="mono ell grow" href={url} target="_blank" rel="noreferrer">{url}</a>
        <button class="btn sm" onclick={copy}><Copy size={13} />Copy</button>
        <a class="btn sm" href={url} target="_blank" rel="noreferrer"><ExternalLink size={13} />Open</a>
      </div>
      <p class="tiny muted">Anyone with the link can read it. It's in the panel's audit log too.</p>
    {:else}
      <p class="small subtle">It's uploaded to <b>mclo.gs</b>. <b class="txt">Anyone with the link can read it</b>, and mclo.gs keeps it until 90 days after it was last opened.</p>
      <label class="row small"><span class="muted lab">Log</span>
        <select class="grow" bind:value={pick} disabled={busy}>
          {#each logs as f (f.path)}<option value={f.path}>{label(f)} · {bytes(f.size)}</option>{/each}
        </select></label>
      {#if !logs.length && !err}<p class="small muted">{loaded ? 'This server has no logs yet.' : 'Loading…'}</p>{/if}
      {#if info}
        <div class="hid">
          <div class="small row"><EyeOff size={13} /><b>Hidden before upload</b></div>
          <div class="row wrap small">
            {#if info.hidden.ips}<span class="chip acc">{info.hidden.ips} IP address{info.hidden.ips === 1 ? '' : 'es'}</span>{/if}
            {#if info.hidden.tokens}<span class="chip acc">{info.hidden.tokens} token{info.hidden.tokens === 1 ? '' : 's'} or password{info.hidden.tokens === 1 ? '' : 's'}</span>{/if}
            {#if info.hidden.paths}<span class="chip acc">server folder</span>{/if}
            {#if info.hidden.users}<span class="chip acc">Windows user name</span>{/if}
            {#if !total}<span class="tiny muted">Nothing found to hide.</span>{/if}
          </div>
          <div class="tiny muted">Player names stay; they help read the log. <button class="lnk" onclick={() => (showText = !showText)}>{showText ? 'Hide' : 'Preview'}</button></div>
          {#if showText}<pre class="pre mono">{info.cut ? '…(start not shown)\n' : ''}{info.text}</pre>{/if}
        </div>
      {/if}
      {#if err}<div class="small err">{err}</div>{/if}
      <div class="row end">
        <button class="btn" disabled={busy} onclick={onClose}>Cancel</button>
        <button class="btn pri" disabled={busy || !info} onclick={share}>{busy ? 'Uploading…' : 'Share publicly'}</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .back { position: fixed; inset: 0; background: #0008; display: flex; justify-content: center; align-items: flex-start; z-index: 60; padding: 72px 16px 16px; overflow: auto; }
  .box { width: min(560px, 100%); display: flex; flex-direction: column; gap: 10px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0; font-size: 14px; display: flex; align-items: center; gap: 7px; }
  .wrap { flex-wrap: wrap; }
  .grow { flex: 1; min-width: 0; }
  .end { justify-content: flex-end; }
  .lab { width: 36px; }
  select { background: var(--base); border: 1px solid var(--line2); border-radius: 6px; padding: 5px 8px; color: var(--text); font: inherit; }
  .hid { background: var(--base); border: 1px solid var(--line); border-radius: 8px; padding: 10px; display: flex; flex-direction: column; gap: 6px; }
  .chip.acc { color: #fb923c; background: var(--acc-dim); }
  .txt { color: var(--text); }
  .lnk { color: var(--acc); font-size: inherit; padding: 0; }
  .pre { max-height: 260px; overflow: auto; font-size: 11px; white-space: pre-wrap; word-break: break-word; background: var(--term); padding: 8px; border-radius: 6px; margin: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .link { background: var(--base); border: 1px solid var(--line); border-radius: 8px; padding: 8px 10px; }
  .err { color: var(--red); }
</style>
