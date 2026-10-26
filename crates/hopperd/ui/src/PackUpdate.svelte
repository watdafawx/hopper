<script>
  // A modpack update for a server, as in Hopper desktop: pick the version
  // (newest by default), read the changelogs since the installed one,
  // compare (the panel downloads the server pack and works out what
  // changes), choose what happens to files both you and the pack changed,
  // then update. Replaced and removed files go to .hopper-backups.
  import { X, ChevronDown, ChevronRight, ArrowRight, TriangleAlert, Package, ServerCog, Info, ExternalLink, FolderOpen } from '@lucide/svelte';
  import { onDestroy, untrack } from 'svelte';
  import { api, post } from './lib/api.js';
  import { followJob } from './lib/jobs.js';
  import { say } from './lib/toast.svelte.js';
  import BrowseRich from './BrowseRich.svelte';

  let { server, check, onClose, onDone = () => {}, openFiles = null } = $props();

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const pack = $derived(check.pack);
  const newer = $derived(check.newer || []);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));

  let pick = $state(untrack(() => check.newer?.[0]?.id ?? ''));
  const target = $derived(newer.find((v) => v.id === pick));
  // Every version after the installed one, up to the picked one (newest first).
  const between = $derived(target ? newer.filter((v) => v.date <= target.date) : []);

  // 'pick' → 'comparing' → 'review' → 'applying' → 'done'
  let phase = $state('pick');
  let status = $state(null);
  let error = $state('');
  let plan = $state(null);
  let jobId = null;
  let take = $state(new Set());
  let bulk = $state('suggested');
  let open = $state({});
  let diffs = $state({});
  let result = $state(null);

  // ---- changelogs ----
  let logs = $state({});
  let logOpen = $state({});
  let mrLogs = null;

  async function loadLog(v) {
    if (logs[v.id]) return;
    logs[v.id] = 'loading';
    try {
      if (pack.source === 'modrinth') {
        mrLogs ??= api(`/browse/versions?source=modrinth&id=${encodeURIComponent(pack.project)}`).then((r) => Object.fromEntries((r.versions || []).map((x) => [x.id, x.changelog])));
        logs[v.id] = (await mrLogs)[v.id] || [];
      } else {
        logs[v.id] = (await api(`/browse/changelog?id=${encodeURIComponent(pack.project)}&file=${encodeURIComponent(v.id)}`)).changelog || [];
      }
    } catch (e) {
      logs[v.id] = { error: e.message };
    }
  }

  function toggleLog(v) {
    logOpen[v.id] = !logOpen[v.id];
    if (logOpen[v.id]) loadLog(v);
  }

  // The picked version's changelog starts open.
  $effect(() => {
    const t = target;
    if (t && logOpen[t.id] === undefined) {
      logOpen[t.id] = true;
      loadLog(t);
    }
  });

  function choose(id) {
    if (id === pick || busy) return;
    pick = id;
    if (phase === 'review') {
      phase = 'pick';
      plan = null;
      error = '';
    }
  }

  // ---- comparing ----
  const why = $derived(
    !pack.updatable
      ? "Modrinth packs don't publish server packs, so Hopper can't update this server in place. Make a new server from the new version (Overview › New server) and move the world over."
      : target && !target.server_files
        ? `The author of ${pack.name} didn't publish a server pack for ${target.name}, so Hopper can't update the server from it. Pick another version.`
        : '',
  );
  const busy = $derived(phase === 'comparing' || phase === 'applying');

  async function compare() {
    error = '';
    plan = null;
    phase = 'comparing';
    status = { detail: 'Starting', progress: null };
    try {
      const { job } = await post(`${base}/pack/plan`, { file: pick });
      jobId = job;
      const r = await followJob(job, (st) => (status = st));
      if (jobId !== job) return;
      plan = r;
      take = new Set(r.take || []);
      const n = conflicts.length;
      bulk = take.size === 0 ? 'mine' : take.size === n ? 'pack' : 'suggested';
      phase = 'review';
    } catch (e) {
      if (phase === 'comparing') {
        error = e.message;
        phase = 'pick';
      }
    } finally {
      status = null;
    }
  }

  async function close() {
    if (phase === 'applying') return;
    if (phase === 'comparing' && jobId) post(`/jobs/${jobId}/cancel`).catch(() => {});
    jobId = null;
    onClose();
  }

  onDestroy(() => {
    if (phase === 'comparing' && jobId) post(`/jobs/${jobId}/cancel`).catch(() => {});
  });

  // ---- the plan ----
  const mods = $derived(plan?.mods || []);
  const files = $derived(plan?.files || []);
  const conflicts = $derived(files.filter((f) => f.action === 'Conflict').map((f) => f.rel));
  const suggested = $derived(conflicts.filter((r) => (plan?.take || []).includes(r)).length);
  const MOD_GROUPS = [
    ['Add', 'Added', 'running', 'mods the pack adds'],
    ['Update', 'Updated', 'lan', 'mods updated'],
    ['Remove', 'Removed', 'crashed', 'mods the pack drops (moved to backups)'],
  ];
  const FILE_GROUPS = [
    ['Add', 'New', 'running', 'new files from the pack'],
    ['Update', 'Updated', 'lan', "files the pack changed and you didn't"],
    ['Merge', 'Merged', 'lan', 'files both changed in different places: combined'],
    ['KeepYours', 'Yours', '', 'files only you changed: kept'],
    ['Remove', 'Removed', 'crashed', 'files the pack dropped (moved to backups)'],
    ['KeepDropped', 'Kept', '', 'files the pack dropped but you changed: kept'],
  ];
  const modGroups = $derived(MOD_GROUPS.map(([k, label, tint, about]) => ({ k, label, tint, about, rows: mods.filter((m) => m.action === k) })).filter((g) => g.rows.length));
  const fileGroups = $derived(FILE_GROUPS.map(([k, label, tint, about]) => ({ k, label, tint, about, rows: files.filter((f) => f.action === k) })).filter((g) => g.rows.length));
  const kept = $derived(plan?.kept || {});
  const nothing = $derived(plan && !mods.length && !files.some((f) => !['KeepYours', 'KeepDropped'].includes(f.action)) && !plan.loader);

  const BULK = $derived([
    ...(suggested > 0 && suggested < conflicts.length ? [['suggested', 'Suggested']] : []),
    ['mine', 'Keep all my changes'],
    ['pack', "Take all the pack's"],
    ['review', 'Review each'],
  ]);
  const mineCount = $derived(conflicts.filter((r) => !take.has(r)).length);
  const bulkNote = $derived(
    {
      suggested: `Pack content (quests, scripts) takes the pack's copy (${conflicts.length - mineCount}); settings keep yours (${mineCount}).`,
      mine: "Your copy of every file stays; the pack's changes to them are skipped.",
      pack: "The pack's copy replaces every file.",
      review: `Picked per file: ${mineCount} keep yours, ${conflicts.length - mineCount} take the pack's.`,
    }[bulk],
  );

  function pickBulk(b) {
    bulk = b;
    if (b === 'suggested') take = new Set(plan.take || []);
    else if (b === 'mine') take = new Set();
    else if (b === 'pack') take = new Set(conflicts);
    else open.Conflict = true;
  }

  function flip(rel, theirs) {
    const s = new Set(take);
    theirs ? s.add(rel) : s.delete(rel);
    take = s;
    bulk = 'review';
  }

  async function toggleDiff(rel) {
    if (diffs[rel] !== undefined) {
      delete diffs[rel];
      diffs = { ...diffs };
      return;
    }
    diffs[rel] = 'loading';
    try {
      const d = (await api(`${base}/pack/diff?plan=${encodeURIComponent(plan.plan)}&rel=${encodeURIComponent(rel)}`)).diff;
      diffs[rel] = d ?? { error: "Not text: can't show the difference." };
    } catch (e) {
      diffs[rel] = { error: e.message };
    }
  }

  const lineKind = (l) => (l.startsWith('+++') || l.startsWith('---') || l.startsWith('@@') ? 'dh' : l.startsWith('+') ? 'da' : l.startsWith('-') ? 'dr' : '');

  // ---- applying ----
  async function update() {
    error = '';
    phase = 'applying';
    status = { detail: 'Starting', progress: null };
    try {
      const { job } = await post(`${base}/pack/apply`, { plan_id: plan.plan, take: [...take] });
      result = await followJob(job, (st) => (status = st));
      phase = 'done';
      say(`${server.name} is on ${result.version}. Old files are in backups.`);
      onDone();
    } catch (e) {
      error = e.message;
      // The plan is used up either way: compare again.
      phase = 'pick';
      plan = null;
    } finally {
      status = null;
    }
  }

  const date = (d) => (d ? new Date(d).toLocaleDateString() : '');
  const typeTint = (t) => (t === 'release' ? '' : t === 'beta' ? 'starting' : 'crashed');
</script>

{#snippet group(key, label, tint, about, count, body)}
  <button class="gh row" onclick={() => (open[key] = !open[key])}>
    {#if open[key]}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}
    <b class="n">{count}</b><span class="chip {tint}">{label}</span><span class="tiny muted ell">{about}</span>
  </button>
  {#if open[key]}<div class="rows">{@render body()}</div>{/if}
{/snippet}

<div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && close()}>
  <div class="box card col">
    <h3 class="row">
      {#if pack.icon}<img class="pi" src={pack.icon} alt="" width="22" height="22" />{:else}<Package size={16} />{/if}
      <span class="ell">Update {server.name}'s pack</span><span class="sp"></span>
      <button class="btn sm ghost icon" title="Close" disabled={phase === 'applying'} onclick={close}><X size={14} /></button>
    </h3>
    <div class="small subtle row wrap">
      <span>{pack.name}</span>
      <span class="vchip"><span class="ell muted">{pack.current.name}</span><ArrowRight size={11} /><span class="ell">{target?.name || '…'}</span></span>
      {#if pack.page}<a class="row tiny" href={pack.page} target="_blank" rel="noreferrer noopener">Pack page<ExternalLink size={11} /></a>{/if}
    </div>

    <div class="scroll col">
      {#if phase !== 'done'}
        <div class="sec">
          <div class="lbl">Version</div>
          <div class="vers">
            {#each newer as v, i (v.id)}
              <button class="ver row" class:on={v.id === pick} disabled={busy} onclick={() => choose(v.id)}>
                <span class="radio" class:on={v.id === pick}></span>
                <span class="ell grow">{v.name}</span>
                {#if i === 0}<span class="chip acc">Newest</span>{/if}
                {#if v.type !== 'release'}<span class="chip {typeTint(v.type)}">{v.type}</span>{/if}
                {#if pack.updatable && !v.server_files}<span class="chip" title="No server pack for this version">No server files</span>{/if}
                <span class="tiny muted">{date(v.date)}</span>
              </button>
            {/each}
          </div>
        </div>

        <div class="sec">
          <div class="lbl">What's new{between.length > 1 ? ` · ${between.length} versions since yours` : ''}</div>
          {#each between as v (v.id)}
            <div class="log">
              <button class="lh row" onclick={() => toggleLog(v)}>
                {#if logOpen[v.id]}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}
                <span class="ell grow">{v.name}</span><span class="tiny muted">{date(v.date)}</span>
              </button>
              {#if logOpen[v.id]}
                <div class="notes small">
                  {#if logs[v.id] === 'loading' || !logs[v.id]}<span class="muted row"><span class="spin"></span>Loading the changelog</span>
                  {:else if logs[v.id].error}<span class="muted">Couldn't load the changelog: {logs[v.id].error}</span>
                  {:else}<BrowseRich blocks={logs[v.id]} empty="No changelog for this version." />{/if}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}

      {#if why}<div class="warn small row"><TriangleAlert size={14} /><span>{why}</span></div>{/if}

      {#if status}
        <div class="sec">
          <div class="small gold row"><span class="spin"></span>{status.detail || (phase === 'applying' ? 'Updating' : 'Comparing')}</div>
          <div class="bar"><i class:ind={status.progress == null} style:width="{status.progress == null ? 30 : Math.max(2, status.progress * 100)}%"></i></div>
          {#if phase === 'comparing'}<div class="tiny muted">The panel downloads the new server pack and compares it with the server's files. Nothing changes yet.</div>{/if}
        </div>
      {/if}

      {#if phase === 'review' && plan}
        <div class="sec col">
          <div class="lbl">What the update changes</div>
          {#if plan.loader}<div class="small row"><ServerCog size={14} class="bluet" />NeoForge {plan.loader.from} → {plan.loader.to}: installed on the server, run script pointed at it.</div>{/if}
          {#if plan.mods_note}<div class="warn small row"><TriangleAlert size={14} /><span>{plan.mods_note}</span></div>{/if}
          {#if plan.base_known && plan.base_assumed}<div class="tiny muted">No record of the server pack this server came from, so it's compared with {plan.base} (the installed version's).</div>{/if}
          {#if !plan.base_known}<div class="tiny muted">The installed version's server pack couldn't be read, so every file that differs from the pack's counts as changed by both, and nothing is removed.</div>{/if}
          {#if nothing}<div class="small subtle">The server already matches {plan.target.name}'s server pack.</div>{/if}

          {#if modGroups.length || kept.added || kept.yours || kept.left_off}
            <div class="sub">Mods</div>
            {#each modGroups as g (g.k)}
              {#snippet modRows()}
                {#each g.rows as m (m.from + '>' + m.to)}
                  <div class="r row">
                    {#if m.icon}<img src={m.icon} alt="" width="18" height="18" loading="lazy" />{:else}<span class="ph"></span>{/if}
                    <span class="ell grow">{m.name}</span>
                    {#if m.action === 'Update'}<span class="vchip"><span class="ell muted">{m.from}</span><ArrowRight size={11} /><span class="ell">{m.to}</span></span>
                    {:else}<span class="tiny muted ell">{m.to || m.from}</span>{/if}
                  </div>
                {/each}
              {/snippet}
              {@render group('m' + g.k, g.label, g.tint, g.about, g.rows.length, modRows)}
            {/each}
            {#if kept.added || kept.yours || kept.left_off}
              <div class="tiny muted kept">
                {[kept.added && `${kept.added} you added stay`, kept.yours && `${kept.yours} keep your version (the pack didn't change them)`, kept.left_off && `${kept.left_off} you left off stay off`].filter(Boolean).join(' · ')}
              </div>
            {/if}
          {/if}

          {#if fileGroups.length || conflicts.length}
            <div class="sub">Config and other files</div>
            {#each fileGroups as g (g.k)}
              {#snippet fileRows()}
                {#each g.rows.slice(0, 500) as f (f.rel)}<div class="r mono tiny ell">{f.rel}</div>{/each}
                {#if g.rows.length > 500}<div class="r tiny muted">and {g.rows.length - 500} more</div>{/if}
              {/snippet}
              {@render group('f' + g.k, g.label, g.tint, g.about, g.rows.length, fileRows)}
            {/each}
          {/if}

          {#if conflicts.length}
            <div class="both col">
              <div class="row"><span class="chip starting">Both changed</span><b class="small">{conflicts.length} file{conflicts.length === 1 ? '' : 's'} you and the pack both changed</b></div>
              <div class="seg">
                {#each BULK as [k, label] (k)}<button class:on={bulk === k} onclick={() => pickBulk(k)}>{label}</button>{/each}
              </div>
              <div class="tiny muted">{bulkNote} Replaced files go to .hopper-backups.</div>
              <button class="lh row small" onclick={() => (open.Conflict = !open.Conflict)}>
                {#if open.Conflict}<ChevronDown size={13} />Hide the files{:else}<ChevronRight size={13} />Review {conflicts.length} file{conflicts.length === 1 ? '' : 's'}{/if}
              </button>
              {#if open.Conflict}
                <div class="rows">
                  {#each conflicts as rel (rel)}
                    {@const theirs = take.has(rel)}
                    <div class="cf">
                      <div class="row">
                        <span class="mono tiny ell grow" title={rel}>{rel}</span>
                        <button class="btn sm ghost" onclick={() => toggleDiff(rel)}>{diffs[rel] !== undefined ? 'Hide diff' : 'Diff'}</button>
                        <div class="seg xs">
                          <button class:on={!theirs} onclick={() => flip(rel, false)}>Keep mine</button>
                          <button class:on={theirs} onclick={() => flip(rel, true)}>Take pack's</button>
                        </div>
                      </div>
                      {#if diffs[rel] !== undefined}
                        {#if diffs[rel] === 'loading'}<div class="tiny muted row"><span class="spin"></span>Loading</div>
                        {:else if diffs[rel].error}<div class="tiny muted">{diffs[rel].error}</div>
                        {:else}<pre class="diff">{#each diffs[rel].split('\n') as l, i (i)}<span class={lineKind(l)}>{l}{'\n'}</span>{/each}</pre>{/if}
                      {/if}
                    </div>
                  {/each}
                </div>
              {/if}
            </div>
          {/if}
          <div class="tiny muted row"><Info size={12} />Worlds, ops, whitelist and bans are never touched. Server packs often leave out global datapacks and configs of mods new to the pack.</div>
        </div>
      {/if}

      {#if phase === 'done' && result}
        <div class="sec col">
          <div class="row"><span class="chip running">Updated</span><b>{server.name} is on {result.version}</b></div>
          <div class="small subtle">{result.summary}</div>
          {#if result.backups}
            <div class="small row wrap">Replaced and removed files are in <span class="mono">{result.backups}</span>
              {#if openFiles}<button class="btn sm" onclick={() => { openFiles(result.backups); onClose(); }}><FolderOpen size={13} />Open</button>{/if}
            </div>
          {/if}
          <div class="tiny muted">Players need the same pack version to join: update their instances too.</div>
        </div>
      {/if}

      {#if error}<div class="small redt row"><TriangleAlert size={14} /><span>{error}</span></div>{/if}
    </div>

    <div class="row end">
      {#if phase === 'review' && alive}<span class="small gold">Stop the server first</span>{/if}
      <span class="sp"></span>
      {#if phase === 'done'}
        <button class="btn pri" onclick={onClose}>Close</button>
      {:else}
        <button class="btn" disabled={phase === 'applying'} onclick={close}>Cancel</button>
        {#if phase === 'review'}
          <button class="btn pri" disabled={alive || nothing} title={alive ? 'Stop the server first' : ''} onclick={update}><ServerCog size={14} />Update server</button>
        {:else}
          <button class="btn pri" disabled={!target || !!why || busy} onclick={compare}>{phase === 'comparing' ? 'Comparing…' : phase === 'applying' ? 'Updating…' : 'Compare'}</button>
        {/if}
      {/if}
    </div>
  </div>
</div>

<style>
  .back { position: fixed; inset: 0; background: #0008; display: flex; justify-content: center; align-items: flex-start; padding: 64px 16px 16px; z-index: 60; }
  .box { width: min(720px, 100%); max-height: calc(100vh - 80px); gap: 10px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0; font-size: 15px; }
  .pi { border-radius: 5px; background: var(--raise); flex: none; }
  .scroll { overflow: auto; min-height: 0; gap: 14px; }
  .sec { display: flex; flex-direction: column; gap: 6px; }
  .sub { font-size: 12.5px; font-weight: 600; margin-top: 6px; }
  .wrap { flex-wrap: wrap; }
  .grow { flex: 1; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .vers { display: flex; flex-direction: column; gap: 2px; max-height: 180px; overflow: auto; }
  .ver { padding: 5px 8px; border-radius: 6px; text-align: left; font-size: 12.5px; }
  .ver:hover:not(:disabled) { background: #ffffff08; }
  .ver.on { background: var(--acc-dim); }
  .radio { width: 12px; height: 12px; border-radius: 99px; border: 1.5px solid var(--line2); flex: none; }
  .radio.on { border-color: var(--acc); background: radial-gradient(var(--acc) 45%, transparent 50%); }
  .log { border-radius: 6px; }
  .lh { gap: 6px; padding: 4px 4px; text-align: left; width: 100%; font-size: 12.5px; color: var(--subtle); }
  .lh:hover { color: var(--text); }
  .notes { margin: 2px 0 4px 20px; background: var(--base); border: 1px solid var(--line); border-radius: 6px; padding: 6px 10px; max-height: 260px; overflow: auto; overflow-wrap: anywhere; }
  .vchip { display: inline-flex; align-items: center; gap: 4px; max-width: 320px; min-width: 0; padding: 1px 7px; border-radius: 6px; background: var(--acc-dim); font-size: 11.5px; color: var(--acc); flex: none; }
  .gh { gap: 8px; padding: 4px; width: 100%; text-align: left; border-radius: 6px; }
  .gh:hover { background: #ffffff06; }
  .gh .n { width: 30px; text-align: right; }
  .rows { display: flex; flex-direction: column; gap: 1px; padding-left: 24px; max-height: 260px; overflow: auto; }
  .r { padding: 2px 4px; gap: 8px; min-width: 0; }
  .r img, .ph { width: 18px; height: 18px; border-radius: 4px; background: var(--raise); flex: none; }
  .kept { padding-left: 28px; }
  .both { border: 1px solid #eab30855; border-radius: 8px; padding: 10px 12px; gap: 8px; }
  .seg { display: inline-flex; flex-wrap: wrap; border: 1px solid var(--line2); border-radius: 7px; overflow: hidden; align-self: flex-start; flex: none; }
  .seg button { padding: 4px 10px; font-size: 12px; color: var(--subtle); border-right: 1px solid var(--line2); }
  .seg button:last-child { border-right: 0; }
  .seg button.on { background: var(--acc-dim); color: var(--acc); }
  .seg.xs button { padding: 2px 8px; font-size: 11.5px; }
  .cf { padding: 3px 0; border-bottom: 1px solid var(--line); }
  .diff { margin: 4px 0; max-height: 240px; overflow: auto; background: var(--term); border: 1px solid var(--line); border-radius: 6px; padding: 6px 8px; font: 11.5px/1.5 var(--mono); white-space: pre; }
  .diff .da { color: var(--green); } .diff .dr { color: var(--red); } .diff .dh { color: var(--blue); }
  .warn { color: var(--gold); gap: 6px; align-items: flex-start; }
  .redt { color: var(--red); gap: 6px; align-items: flex-start; }
  .gold { color: var(--gold); }
  .bar { height: 6px; border-radius: 3px; background: var(--acc-dim); overflow: hidden; }
  .bar i { display: block; height: 100%; background: var(--acc); transition: width .3s; }
  .bar i.ind { animation: slide 1.2s ease-in-out infinite; }
  @keyframes slide { from { transform: translateX(-100%); } to { transform: translateX(340%); } }
  .spin { width: 11px; height: 11px; border: 2px solid currentColor; border-right-color: transparent; border-radius: 99px; animation: rot .8s linear infinite; flex: none; }
  @keyframes rot { to { transform: rotate(360deg); } }
  .end { justify-content: flex-end; }
  .icon { padding: 3px 5px; }
  :global(.bluet) { color: var(--blue); }
</style>
