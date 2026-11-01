<script>
  // Add content › From GitHub or a link…, and a mod's Follow a source…,
  // as on desktop: paste a link, Hopper finds the file for this server (or
  // lists them when it can't tell), says who publishes it the first time,
  // then adds it. Updates come through the normal update check.
  import { X, Package, CircleCheck, Circle, CircleX, Info } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { dialog } from './lib/dialog.svelte.js';

  // follow: the installed mod to follow a source (null adds a new one).
  let { server, follow = null, onClose, onChanged = () => {} } = $props();
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));

  let allowed = $state(null);
  let link = $state('');
  let pre = $state(false);
  let looking = $state(false);
  let error = $state('');
  let found = $state(null);
  let chosen = $state(null);
  let picking = $state(false);
  let busy = $state('');
  // Following a source with the jar already there: how it matches.
  let plan = $state(null);

  const size = (n) => (!n ? '' : n < 1 << 20 ? `${Math.ceil(n / 1024)} KB` : `${(n / (1 << 20)).toFixed(1)} MB`);
  const tagClass = (c) => (c.good ? 'okc' : c.fits ? 'info' : c.tag === 'Source code' || c.tag === 'Docs' || c.tag === 'API only' || c.tag === 'Dev build' || c.tag === 'Not a jar' ? '' : 'bad');
  const pick = $derived(found && chosen != null ? found.candidates[chosen] : null);

  async function loadAllowed() {
    try {
      allowed = (await api('/settings/sources')).allowed;
    } catch {
      allowed = false;
    }
  }

  async function turnOn() {
    try {
      await api('/settings/sources', { method: 'PUT', body: { allowed: true } });
      allowed = true;
    } catch (e) {
      say(e.message, true);
    }
  }

  async function lookUp() {
    if (!link.trim() || looking) return;
    looking = true;
    error = '';
    found = null;
    plan = null;
    chosen = null;
    picking = false;
    try {
      if (follow) {
        // The installed jar is matched where it is; nothing downloads.
        plan = await post(`${base}/mods/source/plan`, { file: follow.file, link: link.trim(), prerelease: pre });
        return;
      }
      const r = await post(`${base}/mods/source/resolve`, { link: link.trim(), prerelease: pre });
      found = r;
      chosen = r.pick;
      picking = r.ambiguous || r.pick == null;
    } catch (e) {
      error = e.message;
    } finally {
      looking = false;
    }
  }

  async function adopt() {
    busy = `Following ${plan.publisher}`;
    try {
      const r = await post(`${base}/mods/source/adopt`, { file: follow.file, link: link.trim(), prerelease: pre });
      say(r.newest ? `${follow.name} follows ${r.publisher}. ${r.newest.version} is out: it's in Updates.` : `${follow.name} follows ${r.publisher}. It's up to date.`);
      onChanged(true);
      onClose();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
    }
  }

  const matchText = (p) =>
    p.matched.kind === 'exact' ? `Your jar is the file from release ${p.matched.tag}, byte for byte.`
    : p.matched.kind === 'baseline' && !p.matched.tag ? `Your jar says it's version ${p.matched.version}. ${p.publisher} has no release that old, so every release there counts as newer.`
    : p.matched.kind === 'baseline' ? `Your jar says it's version ${p.matched.version}; the nearest release is ${p.matched.tag}. Only later releases count as updates.`
    : "Your jar doesn't say its version, so the newest release counts as an update.";

  async function suggest() {
    if (!follow || link) return;
    try {
      const r = await api(`${base}/mods/source/suggest?file=${encodeURIComponent(follow.file)}`);
      if (r.link && !link) link = r.link;
    } catch {}
  }

  async function add() {
    if (!pick) return;
    busy = follow ? `Following ${found.publisher}` : `Adding ${pick.file}`;
    try {
      const { job } = await post(`${base}/mods/source/add`, { link: link.trim(), prerelease: pre, file: pick.file, follow: follow?.file || null });
      await followJob(job, (st) => (busy = st.detail || busy));
      say(`${pick.file} is in. Updates come from ${found.publisher}.${alive ? ' Restart the server to apply.' : ''}`);
      onChanged(true);
      onClose();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
    }
  }

  loadAllowed();
  suggest();
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && !dialog.open && onClose()} />

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && onClose()}>
  <div class="box popover col" role="dialog" aria-label={follow ? `Follow a source for ${follow.name}` : 'Add from GitHub or a link'}>
    <h3 class="row">{follow ? `Follow a source for ${follow.name}` : 'Add from GitHub or a link'}<span class="sp"></span><button class="btn sm ghost icon" title="Close" onclick={onClose}><X size={14} /></button></h3>
    {#if allowed === false}
      <div class="col gap small">
        <p>Hopper Server can add mods from a GitHub release, a Maven repo, a Jenkins job or a download link, and keep them updated from there.</p>
        <p>These files skip CurseForge's and Modrinth's checks. Hopper shows who publishes each one before it adds it, and never runs anything from a download.</p>
        <p class="subtle">You can turn this off again in Settings › Mod sources.</p>
      </div>
      <div class="row end"><button class="btn sm" onclick={onClose}>Cancel</button><button class="btn sm pri" onclick={turnOn}>Turn on</button></div>
    {:else if allowed}
      {#if follow && (follow.source === 'curseforge' || follow.source === 'modrinth')}
        {@const site = follow.source === 'modrinth' ? 'Modrinth' : 'CurseForge'}
        <div class="row small note"><Info size={14} /><span>This mod comes from {site} now. Once it follows a source, its updates come from there instead of {site}. Stop following puts it back.</span></div>
      {/if}
      <label class="col lbl small subtle">A GitHub repo or release, a Maven repo, a Jenkins job, or a link to a .jar
        <form class="row" onsubmit={(e) => { e.preventDefault(); lookUp(); }}>
          <div class="input grow"><input bind:value={link} placeholder="https://github.com/owner/repo" autocomplete="off" spellcheck="false" oninput={() => { found = null; plan = null; error = ''; }} /></div>
          <button class="btn sm" disabled={!link.trim() || looking}>{looking ? 'Looking…' : 'Look up'}</button>
        </form>
      </label>
      <label class="row small"><input type="checkbox" bind:checked={pre} onchange={() => (found || plan) && lookUp()} />Include pre-releases</label>
      {#if looking}<div class="skel"></div><div class="skel"></div>{/if}
      {#if error}<div class="row small err"><CircleX size={14} /><span>{error}</span></div>{/if}
      {#if plan}
        <div class="file col">
          <div class="row"><b class="ell">{plan.name}</b><span class="chip tag info">{plan.kind}</span></div>
          <div class="small">{matchText(plan)}</div>
          <div class="tiny muted">{plan.newest ? `${plan.newest.version} is out (${plan.newest.file}). It shows in Updates; nothing changes until you update.` : "It's up to date. Later releases will show in Updates."}</div>
        </div>
        {#if !plan.trusted}
          <div class="warn small"><b>First file from {plan.publisher}</b><div>Published by {plan.publisher}. Files from {plan.kind} aren't checked by CurseForge or Modrinth. Add them only from people you trust. Hopper never runs anything from the download.</div></div>
        {/if}
        <div class="tiny muted">Your jar stays as it is. Stop following puts the mod back to where it came from.</div>
      {/if}
      {#if found}
        {#if !found.candidates.length}
          <div class="small subtle">This source has no jar files for Hopper to add.</div>
        {:else if picking}
          <div class="small subtle">{found.pick == null ? "None of these say they fit this server. Pick one only if you know it does; Hopper reads the jar after download and stops if it doesn't." : 'Several files fit. Pick the one you want; updates keep to the same kind of file.'}</div>
          <div class="list">
            {#each found.candidates as c, i (c.file)}
              <button class="it row-hover" class:row-sel={chosen === i} onclick={() => (chosen = i)}>
                {#if chosen === i}<CircleCheck size={14} />{:else}<Circle size={14} />{/if}
                <span class="ell grow">{c.file}</span>
                <span class="chip tag {tagClass(c)}">{c.tag}</span>
                <span class="tiny muted sz">{size(c.size)}</span>
              </button>
            {/each}
          </div>
        {:else if pick}
          <div class="file row">
            <Package size={18} />
            <div class="col grow min0">
              <div class="row"><b class="ell">{pick.file}</b><span class="chip tag info">{found.kind}</span></div>
              <div class="tiny muted">{[pick.title, size(pick.size), pick.good ? 'fits this server' : 'Hopper checks the jar after download'].filter(Boolean).join(' · ')}</div>
            </div>
            {#if found.candidates.length > 1}<button class="btn sm ghost" onclick={() => (picking = true)}>Other file…</button>{/if}
          </div>
        {/if}
        {#if pick && !found.trusted}
          <div class="warn small">
            <b>First file from {found.publisher}</b>
            <div>{pick.author ? `Published by ${found.publisher}, release by ${pick.author}.` : `Published by ${found.publisher}.`} Files from {found.kind} aren't checked by CurseForge or Modrinth. Add them only from people you trust. Hopper never runs anything from the download.</div>
          </div>
        {/if}
      {/if}
      <div class="row end">
        {#if busy}<span class="tiny muted ell">{busy}</span>{/if}
        <button class="btn sm" onclick={onClose}>Cancel</button>
        {#if plan}
          <button class="btn sm pri" disabled={!!busy} onclick={adopt}>{plan.trusted ? 'Follow' : 'Trust and follow'}</button>
        {:else if found}
          <button class="btn sm pri" disabled={!pick || !!busy} onclick={add}>{follow ? (found.trusted ? 'Follow' : 'Trust and follow') : found.trusted ? 'Add' : 'Trust and add'}</button>
        {:else}
          <button class="btn sm pri" disabled={!link.trim() || looking} onclick={lookUp}>Look up</button>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .box { width: min(560px, 100%); max-height: calc(100vh - var(--dialog-top) - 16px); gap: 12px; padding: 18px; overflow: auto; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; display: flex; align-items: center; gap: 8px; }
  .box p { margin: 0; }
  .gap { gap: 8px; }
  .lbl { gap: 6px; }
  .grow { flex: 1; min-width: 0; }
  .min0 { min-width: 0; gap: 2px; }
  .input.grow { padding: 5px 9px; }
  .end { justify-content: flex-end; gap: 8px; }
  .list { overflow: auto; display: flex; flex-direction: column; gap: 2px; min-height: 0; max-height: 260px; }
  .it { display: flex; align-items: center; gap: 8px; padding: 6px 8px; border-radius: var(--radius); text-align: left; font-size: 12.5px; }
  .sz { width: 56px; text-align: right; }
  .file { gap: 10px; padding: 10px 12px; border-radius: var(--radius-lg); background: var(--panel-2, rgba(255,255,255,.04)); }
  .warn { display: flex; flex-direction: column; gap: 4px; padding: 10px 12px; border-radius: var(--radius-lg); background: var(--gold-dim, rgba(240,180,60,.12)); }
  .err { color: var(--red, #f87171); gap: 6px; }
  .note { gap: 8px; align-items: flex-start; padding: 8px 10px; border-radius: var(--radius-lg); background: var(--blue-dim); }
  .skel { height: 34px; border-radius: var(--radius); background: rgba(255,255,255,.05); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .icon { padding: 3px 5px; }
</style>
