<script>
  // Settings › Version: what the server runs, and changing it. Same two
  // pickers as desktop's instance settings (Minecraft, loader). Picking
  // something else checks it (Java, the loader's changelog, mods on
  // CurseForge / Modrinth for a new Minecraft), then one button backs the
  // whole server up and changes it. The result, with Undo, stays here
  // until the next change; the backup is in Backups too.
  import { RotateCcw, Play, ArrowRightLeft, Check, CircleAlert, Info, Archive, ChevronDown, ChevronRight } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { bytes } from './lib/format.js';
  import { power } from './lib/power.js';

  let { server } = $props();
  const sid = $derived(server.id);
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));

  const NAMES = { vanilla: 'Vanilla', neoforge: 'NeoForge', forge: 'Forge', fabric: 'Fabric', quilt: 'Quilt', paper: 'Paper', purpur: 'Purpur' };
  let v = $state(null);
  let versions = $state([]);
  let loaders = $state([]);
  let mc = $state('');
  let lv = $state('');
  let check = $state(null);
  let checking = $state('');
  let checkErr = $state('');
  let busy = $state('');
  let showChanges = $state(false);
  let modFilter = $state('none');
  let update = $state(true);
  let offNone = $state(true);
  let offUnknown = $state(false);
  let checkSeq = 0;

  const have = $derived(v?.installed);
  const kind = $derived(have?.kind || '');
  const label = (k, ver) => `${NAMES[k] || k} ${ver || ''}`.trim();
  // Fabric and Quilt servers that haven't run don't show their loader
  // version yet: what the list picked first stands in for it.
  let autoLv = $state('');
  const installedLv = $derived(have?.version || autoLv);
  const changed = $derived(!!have && (mc !== have.minecraft || (kind !== 'vanilla' && !!lv && lv !== installedLv)));
  const fits = $derived(check?.mods || []);
  const count = (f) => fits.filter((m) => m.fit === f).length;
  const shownMods = $derived(modFilter === 'all' ? fits : fits.filter((m) => m.fit === modFilter));
  const newer = $derived(have && kind !== 'vanilla' && have.minecraft === mc ? loaders.indexOf(have.version) : -1);

  async function load() {
    try {
      v = await api(`${base}/version`);
      if (!mc) mc = v.installed.minecraft;
      if (!lv) lv = v.installed.version;
    } catch (e) {
      say(e.message, true);
    }
  }

  async function loadLoaders(m) {
    loaders = [];
    if (!m || !kind || kind === 'vanilla') return;
    try {
      const r = await api(`/meta/loaders/${encodeURIComponent(m)}`);
      if (m !== mc) return;
      loaders = r.loaders?.[kind] || [];
      // Another Minecraft: its recommended build; same one: what's installed.
      if (m === have.minecraft && loaders.includes(have.version)) {
        if (!loaders.includes(lv)) lv = have.version;
      } else if (!loaders.includes(lv)) {
        lv = r.recommended?.[kind] || loaders[0] || '';
      }
      if (m === have.minecraft && !have.version && !autoLv) autoLv = lv;
    } catch {
      loaders = [];
    }
  }

  async function runCheck() {
    const seq = ++checkSeq;
    check = null;
    checkErr = '';
    if (!changed) return;
    checking = 'Checking';
    try {
      const { job } = await post(`${base}/version/check`, { minecraft: mc, loader_version: kind === 'vanilla' ? '' : lv });
      const r = await followJob(job, (st) => seq === checkSeq && (checking = st.detail || 'Checking'));
      if (seq !== checkSeq) return;
      check = r;
      modFilter = r.mods.some((m) => m.fit === 'none') ? 'none' : r.mods.some((m) => m.fit === 'unknown') ? 'unknown' : 'all';
    } catch (e) {
      if (seq === checkSeq) checkErr = e.message;
    } finally {
      if (seq === checkSeq) checking = '';
    }
  }

  async function change() {
    busy = 'Starting';
    try {
      const { job } = await post(`${base}/version/change`, {
        minecraft: mc,
        loader_version: kind === 'vanilla' ? '' : lv,
        mods: check?.same_minecraft ? [] : fits,
        update_mods: update,
        off_none: offNone,
        off_unknown: offUnknown,
      });
      const r = await followJob(job, (st) => (busy = st.detail || 'Working'));
      say(r.checks.every((c) => c.ok) ? `${server.name} is on ${label(r.to.kind, r.to.version)}, Minecraft ${r.to.minecraft}.` : 'Changed, but a check failed: see Version.', !r.checks.every((c) => c.ok));
      check = null;
      mc = '';
      lv = '';
      await load();
    } catch (e) {
      say(e.message, true);
      await load();
    } finally {
      busy = '';
    }
  }

  async function undo() {
    const rec = v.record;
    const ok = await confirmIt(`Undo the change to ${label(rec.to.kind, rec.to.version)}?`, {
      body: `${server.name} goes back to ${label(rec.from.kind, rec.from.version)} on Minecraft ${rec.from.minecraft}: every file from the backup made before the change, ${rec.ran_since ? 'the world too (it has run since: the current world is kept aside in the backups folder)' : 'the world too'}. Mods added since move to .hopper-backups.`,
      ok: 'Undo',
      danger: rec.ran_since,
    });
    if (!ok) return;
    busy = 'Starting';
    try {
      const { job } = await post(`${base}/version/undo`);
      const r = await followJob(job, (st) => (busy = st.detail || 'Restoring'));
      say(`${server.name} is back on ${r.label}, Minecraft ${r.minecraft}.`);
      check = null;
      mc = '';
      lv = '';
      await load();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
    }
  }

  async function startWatch() {
    await power(server, 'start');
    location.hash = `/server/${encodeURIComponent(server.id)}/console`;
  }

  $effect(() => {
    sid;
    v = null;
    mc = '';
    lv = '';
    check = null;
    load();
    api('/meta/minecraft').then((r) => (versions = r.versions), () => {});
  });
  $effect(() => {
    if (have) loadLoaders(mc);
  });
  // Re-check when the pick settles.
  $effect(() => {
    mc;
    lv;
    if (!have) return;
    const t = setTimeout(runCheck, 400);
    return () => clearTimeout(t);
  });
</script>

{#if !v}
  <div class="small muted">Loading…</div>
{:else}
  {#if v.record}
    {@const r = v.record}
    <h4>Last change</h4>
    <div class="box" class:good={r.ok} class:warnb={!r.ok}>
      <div class="row wrap">
        <b>Now {label(r.to.kind, r.to.version)} · Minecraft {r.to.minecraft}</b>
        <span class="muted small">was {label(r.from.kind, r.from.version)} · {r.from.minecraft}</span>
        <span class="sp"></span>
        {#if !alive}<button class="btn sm go" disabled={!!busy} onclick={startWatch}><Play size={13} />Start and watch</button>{/if}
        <button class="btn sm" disabled={!!busy || alive} title={alive ? 'Stop the server first' : ''} onclick={undo}><RotateCcw size={13} />Undo</button>
      </div>
      <div class="checks">
        {#each r.checks as c, i (i)}<div class:bad={!c.ok}>{#if c.ok}<Check size={13} />{:else}<CircleAlert size={13} />{/if}{c.text}</div>{/each}
        {#each r.notes as n, i (i)}<div class="muted"><Info size={13} />{n}</div>{/each}
      </div>
      <div class="tiny muted">Undo puts back every file from <span class="mono">{r.backup}</span>{r.ran_since ? ', the world too: it has run since the change' : ''}. It's in Backups as well.</div>
    </div>
  {/if}

  <h4>Installed</h4>
  <div class="fl">
    <div class="lab"><div>Minecraft</div><div class="tiny muted">{v.pack ? 'From a modpack server pack: a new Minecraft comes with a pack update' : 'From the start script and the server’s files'}</div></div>
    <div class="ctl">
      <select class="sel" bind:value={mc} disabled={!!busy || v.pack || !kind} class:dirty={have && mc !== have.minecraft}>
        {#if have.minecraft && !versions.includes(have.minecraft)}<option value={have.minecraft}>{have.minecraft}</option>{/if}
        {#each versions as m (m)}<option value={m}>{m}{m === have.minecraft ? ' · installed' : ''}</option>{/each}
      </select>
    </div>
  </div>
  {#if kind && kind !== 'vanilla'}
    <div class="fl">
      <div class="lab"><div>{NAMES[kind] || kind}</div><div class="tiny muted">{have.version ? `${label(kind, have.version)} installed` : `${NAMES[kind] || kind}: its version shows after the first start`}{newer > 0 ? ` · ${newer} newer build${newer === 1 ? '' : 's'}` : ''}</div></div>
      <div class="ctl">
        {#if loaders.length}
          <select class="sel" bind:value={lv} disabled={!!busy} class:dirty={lv !== have.version}>
            {#each loaders as o, i (o)}<option value={o}>{o}{o === have.version && mc === have.minecraft ? ' · installed' : i === 0 ? ' · newest' : ''}</option>{/each}
          </select>
        {:else}<span class="small muted">{mc ? `No ${NAMES[kind]} list for Minecraft ${mc}` : ''}</span>{/if}
      </div>
    </div>
  {:else if !kind}
    <div class="small warn">Hopper can't tell what this server runs from its start script, so it can't change its version here.</div>
  {/if}

  {#if changed}
    <h4>Before you change</h4>
    <div class="box">
      {#if checking}<div class="busy small"><span class="spin"></span>{checking}…</div>
      {:else if checkErr}<div class="err">{checkErr}</div>
      {:else if check}
        <div class="checks">
          <div class:bad={!check.java.ok}>{#if check.java.ok}<Check size={13} />{:else}<Info size={13} />{/if}
            {check.java.ok ? (check.java.bare ? `Plain java: Hopper picks a Java ${check.java.need} at the start` : `Java ${check.java.have} fits Minecraft ${mc}`)
              : `Minecraft ${mc} needs Java ${check.java.need}: ${check.java.installed ? `Hopper switches the start script to the installed Java ${check.java.installed}` : `Hopper downloads Java ${check.java.need} (Temurin)`}`}</div>
          {#if check.same_minecraft}
            <div><Check size={13} />{check.mod_count} mod{check.mod_count === 1 ? '' : 's'} stay on Minecraft {mc}: nothing to check</div>
          {:else}
            <div class:bad={count('none') > 0}>{#if count('none')}<CircleAlert size={13} />{:else}<Check size={13} />{/if}{fits.length} mods checked on CurseForge and Modrinth for {mc}</div>
          {/if}
          {#if check.changes.length}
            <div><Info size={13} />What changed {have.version} → {lv}: {check.changes.length} changelog entr{check.changes.length === 1 ? 'y' : 'ies'}
              <button class="link" onclick={() => (showChanges = !showChanges)}>{#if showChanges}<ChevronDown size={12} />Hide{:else}<ChevronRight size={12} />Show{/if}</button></div>
            {#if showChanges}<div class="log mono">{#each check.changes as c (c.version)}<div><b>{c.version}</b> {c.text}</div>{/each}</div>{/if}
          {/if}
          <div class="muted"><Archive size={13} />A full backup first ({bytes(check.backup_size)}), kept until you delete it</div>
        </div>

        {#if !check.same_minecraft && fits.length}
          <div class="row wrap chips">
            <button class="chip r" class:on={modFilter === 'none'} onclick={() => (modFilter = 'none')}>{count('none')} have none</button>
            <button class="chip y" class:on={modFilter === 'update'} onclick={() => (modFilter = 'update')}>{count('update')} need another file</button>
            <button class="chip g" class:on={modFilter === 'fits'} onclick={() => (modFilter = 'fits')}>{count('fits')} fit as they are</button>
            <button class="chip" class:on={modFilter === 'unknown'} onclick={() => (modFilter = 'unknown')}>{count('unknown')} unknown</button>
            <button class="chip" class:on={modFilter === 'all'} onclick={() => (modFilter = 'all')}>All {fits.length}</button>
          </div>
          <div class="mods">
            {#each shownMods as m (m.file)}
              <div class="mr">
                {#if m.icon}<img src={m.icon} alt="" width="22" height="22" loading="lazy" />{:else}<span class="ph"></span>{/if}
                <div class="ell"><span>{m.name}</span> <span class="tiny muted">{m.fit === 'update' ? `→ ${m.new_file}` : m.fit === 'none' ? `no file for ${mc} yet` : m.fit === 'unknown' ? 'not on CurseForge or Modrinth (or not looked up yet)' : m.file}</span></div>
                <span class="chip {m.fit === 'none' ? 'r' : m.fit === 'update' ? 'y' : m.fit === 'fits' ? 'g' : ''}">{m.fit === 'none' ? 'none' : m.fit === 'update' ? 'update' : m.fit === 'fits' ? 'fits' : 'unknown'}</span>
              </div>
            {:else}<div class="small muted pad">Nothing here.</div>{/each}
          </div>
          <div class="opts small">
            <label><input type="checkbox" bind:checked={update} /> Update the {count('update')} mods that have a file for {mc}</label>
            <label><input type="checkbox" bind:checked={offNone} /> Turn off the {count('none')} with none (Undo brings them back)</label>
            <label><input type="checkbox" bind:checked={offUnknown} /> Turn off the {count('unknown')} unknown</label>
            <div class="muted">A world opened on {mc} can't go back to {have.minecraft}: only the backup can.</div>
          </div>
        {/if}
      {/if}
    </div>
    {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
    <div class="row go-row">
      {#if alive}<span class="small warn">Stop the server first.</span>{/if}
      <span class="sp"></span>
      <button class="btn sm ghost" disabled={!!busy} onclick={() => { mc = have.minecraft; lv = have.version; }}>Cancel</button>
      <button class="btn pri" disabled={!!busy || !!checking || !check || alive} onclick={change}><ArrowRightLeft size={14} />Back up and change to {kind === 'vanilla' ? `Minecraft ${mc}` : mc !== have.minecraft ? `${label(kind, lv)} on ${mc}` : label(kind, lv)}</button>
    </div>
  {:else if busy}
    <div class="busy small"><span class="spin"></span>{busy}</div>
  {/if}
{/if}

<style>
  .fl { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 18px; align-items: center; padding: 10px 4px; border-bottom: 1px solid var(--line); }
  .lab > div:first-child { font-weight: 500; }
  .ctl { display: flex; align-items: center; gap: 8px; justify-content: flex-end; min-width: 0; }
  .sel { background: var(--base); border: 1px solid var(--line2); border-radius: 6px; padding: 5px 8px; color: var(--text); min-width: 200px; }
  .sel.dirty { border-color: var(--acc); }
  .box { border: 1px solid var(--line); background: var(--base); border-radius: 10px; padding: 10px 12px; margin-top: 6px; display: flex; flex-direction: column; gap: 8px; }
  .box.good { border-color: #22c55e55; background: #22c55e0c; }
  .box.warnb { border-color: #eab30855; background: #eab3080a; }
  .wrap { flex-wrap: wrap; gap: 8px; }
  .checks { display: flex; flex-direction: column; gap: 5px; font-size: 13px; }
  .checks > div { display: flex; align-items: center; gap: 7px; flex-wrap: wrap; }
  .checks :global(svg) { flex: none; color: var(--green); }
  .checks .bad, .checks .bad :global(svg) { color: var(--gold); }
  .checks .muted :global(svg) { color: var(--muted); }
  .log { max-height: 220px; overflow: auto; font-size: 11.5px; background: var(--term); border: 1px solid var(--line); border-radius: 8px; padding: 8px 10px; color: var(--subtle); }
  .log b { color: var(--text); font-weight: 600; margin-right: 6px; }
  .link { color: var(--acc); display: inline-flex; align-items: center; gap: 2px; }
  .chips { margin-top: 2px; }
  .chip { cursor: pointer; }
  .chip.on { outline: 1px solid var(--acc); }
  .chip.r { color: #f87171; } .chip.y { color: var(--gold); } .chip.g { color: var(--green); }
  .mods { border: 1px solid var(--line); border-radius: 8px; max-height: 300px; overflow: auto; }
  .mr { display: grid; grid-template-columns: 22px minmax(0, 1fr) auto; gap: 10px; align-items: center; padding: 5px 10px; border-bottom: 1px solid var(--line); }
  .mr:last-child { border: 0; }
  .mr img, .ph { width: 22px; height: 22px; border-radius: 4px; background: var(--raise); display: block; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pad { padding: 8px 10px; }
  .opts { display: flex; flex-direction: column; gap: 4px; border: 1px solid #eab30855; background: #eab3080a; border-radius: 8px; padding: 8px 10px; }
  .opts input { accent-color: var(--acc); }
  .go-row { margin-top: 10px; gap: 8px; }
  .warn { color: var(--gold); }
  .err { color: var(--red); }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); margin-top: 8px; }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; }
  @keyframes s { to { transform: rotate(360deg); } }
  @media (max-width: 900px) { .fl { grid-template-columns: 1fr; } .ctl { justify-content: flex-start; } .sel { width: 100%; } }
</style>
