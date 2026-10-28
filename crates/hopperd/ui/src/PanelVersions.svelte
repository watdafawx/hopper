<script>
  // Hopper Server's versions, opened from the version beside the logo, as
  // Hopper desktop's "Hopper versions" window (src/gpui/hopper_versions.rs):
  // every release on the left; on the right, the picked one's notes and
  // what changes next to the running version, with "Update to X" or "Roll
  // back to X" (owner only, a rollback asks first). Rolling back makes
  // auto-update skip the version it came from; a newer release still
  // installs.
  import { X, RefreshCw, ExternalLink, Download, Undo2, ChevronDown } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { session } from './lib/me.svelte.js';
  import { confirmIt, dialog } from './lib/dialog.svelte.js';
  import { md } from './lib/md.js';
  import { upd, loadUpdate, isNewer } from './lib/panelupdate.svelte.js';

  const PAGE = 10;
  const REPO_PAGE = 'https://github.com/watdafawx/hopper/releases';

  let list = $state(null);
  let error = $state('');
  let checking = $state(false);
  let picked = $state('');
  let shown = $state(PAGE);

  const st = $derived(upd.st);
  const cur = $derived(st?.current || '');
  const newest = $derived(list?.[0]?.version || '');
  // What a rollback keeps auto-update from installing again (see skip_after).
  const top = $derived([newest, st?.skip].filter(Boolean).reduce((a, b) => (isNewer(b, a) ? b : a), cur));
  const canChange = $derived(session.owner && !st?.why_not);
  const running = $derived(['downloading', 'waiting', 'restarting'].includes(st?.state));

  async function load(fresh = false) {
    error = '';
    if (fresh) checking = true;
    try {
      const r = await api(`/panel/releases${fresh ? '?fresh=1' : ''}`);
      list = r.releases;
      // An update on offer opens on the newest release.
      if (!picked || (picked === r.current && list[0] && isNewer(list[0].version, r.current))) {
        picked = list[0] && isNewer(list[0].version, r.current) ? list[0].version : r.current;
      }
      loadUpdate();
    } catch (e) {
      error = e.message;
    }
    checking = false;
  }
  load();

  const close = () => (upd.open = false);
  const day = (iso) => (iso || '').split('T')[0];
  const plural = (n) => `${n} release${n === 1 ? '' : 's'}`;

  // The running version stays in reach even when it's old.
  const need = $derived(list ? list.findIndex((r) => r.version === cur || r.version === picked) + 1 : 0);
  const rows = $derived(list ? list.slice(0, Math.max(shown, need)) : []);
  const more = $derived(list ? list.length - rows.length : 0);

  const pick = $derived(list?.find((r) => r.version === picked) || null);
  const up = $derived(pick ? isNewer(pick.version, cur) : false);
  // An update: the releases between the running one and it. Going back:
  // everything after it, up to the newest.
  const changes = $derived(
    !pick || pick.version === cur ? []
      : up ? list.filter((r) => isNewer(r.version, cur) && !isNewer(r.version, pick.version))
        : list.filter((r) => isNewer(r.version, pick.version)),
  );

  async function install() {
    const v = pick.version;
    if (!up) {
      const ok = await confirmIt(`Roll back to Hopper Server ${v}?`, {
        body: `The panel downloads ${v}, puts it in place and restarts. Minecraft servers keep running. "Update by itself" won't install ${top} or older again; the next new release still installs.`,
        ok: 'Roll back',
        danger: true,
      });
      if (!ok) return;
    }
    try {
      await post('/panel/update/install', { version: v });
      if (upd.st) {
        upd.st.state = 'downloading';
        upd.st.target = v;
      }
      loadUpdate();
      close();
    } catch (e) {
      say(e.message, true);
    }
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && !e.defaultPrevented && !dialog.open && (e.preventDefault(), close())} />

{#snippet card(r)}
  <div class="card" class:you={r.version === cur}>
    <div class="row ch">
      <b>Hopper Server {r.version}</b>
      <span class="tiny muted">{day(r.published)}</span>
      {#if r.version === cur}<span class="chip acc xs">You're on this</span>{/if}
      <span class="sp"></span>
      {#if r.page}<a class="btn sm ghost icon" href={r.page} target="_blank" rel="noreferrer noopener" title="This release on GitHub"><ExternalLink size={13} /></a>{/if}
    </div>
    {#if r.notes?.trim()}<div class="md small">{@html md(r.notes)}</div>
    {:else}<span class="small muted"><i>No notes for this release.</i></span>{/if}
  </div>
{/snippet}

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && close()}>
  <div class="box popover col" role="dialog" aria-label="Hopper Server versions">
    <h3 class="row">Hopper Server versions<span class="sp"></span><button class="btn sm ghost icon" title="Close" onclick={close}><X size={14} /></button></h3>
    <div class="row hd">
      <img class="lg" src="/logo.svg" alt="" width="32" height="32" />
      <div class="col g2">
        <b>Hopper Server {cur}</b>
        <span class="tiny muted">The version this panel runs</span>
      </div>
      {#if running}<span class="chip lan">{st.state === 'restarting' ? 'Restarting…' : `Installing ${st.target || ''}…`}</span>
      {:else if st?.state === 'ready'}<span class="chip running">{st.target} on next start</span>
      {:else if st?.latest}<span class="chip acc">{st.latest} is out</span>
      {:else if list}<span class="chip running">Up to date</span>{/if}
      <span class="sp"></span>
      {#if session.owner}<button class="btn sm ghost" disabled={checking} onclick={() => load(true)}><RefreshCw size={13} />{checking ? 'Checking…' : 'Check for updates'}</button>{/if}
      <a class="btn sm ghost" href={REPO_PAGE} target="_blank" rel="noreferrer noopener"><ExternalLink size={13} />Releases on GitHub</a>
    </div>

    {#if error}
      <div class="col err">
        <span class="small red">Couldn't read the releases: {error}</span>
        <button class="btn sm" onclick={() => load()}><RefreshCw size={13} />Try again</button>
      </div>
    {:else if !list}
      <div class="row small muted loading"><span class="spinner"></span>Reading Hopper Server's releases…</div>
    {:else}
      <div class="cols">
        <div class="list col">
          {#each rows as r (r.version)}
            <button class="vrow row" class:row-sel={r.version === picked} class:row-hover={r.version !== picked} onclick={() => (picked = r.version)}>
              <span class="col g2 grow">
                <span class="v" class:old={isNewer(cur, r.version) && r.version !== picked}>{r.version}</span>
                <span class="tiny muted">{day(r.published)}</span>
              </span>
              {#if r.version === newest}<span class="chip running xs">Latest</span>{/if}
              {#if r.version === cur}<span class="chip acc xs">Yours</span>{/if}
            </button>
          {/each}
          {#if more > 0}
            <button class="btn sm ghost more" title="{more} older releases" onclick={() => (shown = rows.length + PAGE)}><ChevronDown size={13} />Show {Math.min(more, PAGE)} more</button>
          {/if}
        </div>
        <div class="vr"></div>
        <div class="det col">
          {#if !pick}
            <span class="small muted">Pick a version to see what changes.</span>
          {:else if pick.version === cur}
            <b>Hopper Server {cur} is the version you're on</b>
            {@render card(pick)}
          {:else}
            <div class="row dh">
              <div class="col grow">
                <b>{up ? 'Update' : 'Roll back'} to {pick.version}</b>
                <span class="small" class:acc={up} class:gold={!up}>
                  {up ? `What you get over ${cur}: ${plural(changes.length)}.` : `What you'd give up compared to the latest (${newest}): ${plural(changes.length)}.`}
                </span>
              </div>
              {#if canChange}
                <button class="btn sm" class:pri={up} class:danger={!up} disabled={running} onclick={install}>
                  {#if up}<Download size={13} />{:else}<Undo2 size={13} />{/if}{up ? 'Update' : 'Roll back'} to {pick.version}
                </button>
              {/if}
            </div>
            <div class="note tiny col">
              {#if st?.why_not}<span>{st.why_not}</span>
              {:else if !session.owner}<span>Only the panel's owner can change its version.</span>
              {:else}
                <span>The panel restarts into it once running jobs finish. Minecraft servers keep running, and the panel finds them again.</span>
                {#if !up}
                  <span>"Update by itself" won't install {top} or older again; the next new release still installs.</span>
                  <span>To come back, click the version beside the logo and update.</span>
                {/if}
              {/if}
            </div>
            {#each changes as r (r.version)}{@render card(r)}{/each}
          {/if}
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .box { width: min(900px, 100%); height: calc(100dvh - var(--dialog-top) - 24px); gap: 14px; padding: 14px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .hd { gap: 12px; flex-wrap: wrap; }
  .lg { width: 32px; height: 32px; image-rendering: pixelated; flex: none; }
  .g2 { gap: 2px; min-width: 0; }
  .grow { flex: 1; min-width: 0; }
  .err { gap: 8px; align-items: flex-start; }
  .red { color: var(--red); }
  .acc { color: var(--acc); }
  .gold { color: var(--gold); }
  .loading { gap: 8px; justify-content: center; padding: 24px 0; }
  .cols { flex: 1; min-height: 0; display: flex; gap: 16px; }
  .list { width: 200px; flex: none; gap: 2px; overflow: auto; padding-right: 6px; }
  .vrow { gap: 6px; padding: 6px 10px; border-radius: var(--radius); text-align: left; width: 100%; }
  .v { font-size: 13px; font-weight: 500; }
  .row-sel .v { color: var(--acc); }
  .v.old { color: var(--muted); }
  .more { margin-top: 4px; align-self: flex-start; }
  .vr { width: 1px; background: var(--line); flex: none; }
  .det { flex: 1; min-width: 0; overflow: auto; gap: 12px; padding-right: 6px; }
  .dh { gap: 12px; }
  .note { gap: 2px; padding: 8px 12px; border-radius: var(--radius-lg); background: var(--surface); color: var(--subtle); }
  .card { border: 1px solid var(--line); border-radius: var(--radius-lg); padding: 10px 12px; display: flex; flex-direction: column; gap: 6px; }
  .card.you { border-color: var(--acc); }
  .ch { gap: 8px; }
  .md { color: var(--subtle); overflow-wrap: anywhere; }
  .md :global(ul) { margin: 4px 0; padding-left: 18px; }
  .md :global(h1), .md :global(h2), .md :global(h3), .md :global(h4) { font-size: 13px; margin: 8px 0 2px; color: var(--text); }
  .md :global(p) { margin: 4px 0; }
  .chip.xs { font-size: 10.5px; padding: 0 6px; }
  .icon { padding: 3px 5px; }
  @media (max-width: 640px) {
    .cols { flex-direction: column; }
    .list { width: auto; max-height: 160px; }
    .vr { display: none; }
  }
</style>
