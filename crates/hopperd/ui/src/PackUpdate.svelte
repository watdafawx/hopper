<script>
  // A server's modpack update, laid out as Hopper desktop's update window:
  // "What's new" (the changelog of every version the update brings, with
  // the pack author's advice on updating picked out) shows at once while
  // the update is prepared; "Changes" holds Your choice, What changes and
  // What stays. By default it's a quick update like the CurseForge app (the
  // server pack's files replace the server's); "Compare with my changes"
  // finds the files edited on the server so they can be kept. Replaced and
  // removed files go to .hopper-backups.
  import { X, ChevronDown, ChevronRight as Caret, TriangleAlert, Package, ServerCog, FileText, Archive, Check,
    ShieldCheck, Settings2, GitCompare, Download, FolderOpen } from '@lucide/svelte';
  import { onMount, onDestroy, untrack } from 'svelte';
  import { api, post } from './lib/api.js';
  import { followJob } from './lib/jobs.js';
  import { say } from './lib/toast.svelte.js';
  import { ago } from './lib/browse.js';
  import { paintDown, paintOver } from './lib/dragpick.svelte.js';
  import BrowseRich from './BrowseRich.svelte';
  import BrowsePack from './BrowsePack.svelte';
  import { dialog } from './lib/dialog.svelte.js';
  import { rightClear } from './lib/rightclear.js';

  // `file`: the pack version to update to (the newest by default);
  // `onOtherVersion`: opens the pack's Versions and changelogs to pick another.
  let { server, check, file = null, onClose, onDone = () => {}, openFiles = null, onOtherVersion = null } = $props();

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const pack = $derived(check.pack);
  const versions = $derived(check.versions?.length ? check.versions : [...(check.newer || []), check.pack.current]);
  const fileId = untrack(() => file ?? check.newer?.[0]?.id ?? check.pack.current.id);
  const target = $derived(versions.find((v) => v.id === fileId));
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const why = $derived(
    !pack.updatable
      ? "Modrinth packs don't publish server packs, so Hopper can't update this server in place. Make a new server from the new version (Overview › New server) and move the world over."
      : target && !target.server_files
        ? `The author of ${pack.name} didn't publish a server pack for ${target.name}, so Hopper can't update the server from it. Pick another version.`
        : '',
  );

  /** 0: What's new, 1: Changes. */
  let tab = $state(0);

  // ---- What's new ----
  let log = $state(null);
  async function loadLog() {
    try {
      log = await api(`${base}/pack/changelog?file=${encodeURIComponent(fileId)}`);
    } catch (e) {
      log = { error: e.message };
    }
  }

  // ---- preparing the update ----
  let compare = $state(false);
  let plan = $state(null);
  let planErr = $state('');
  let status = $state(null);
  let jobId = null;
  let take = $state(new Set());
  let bulk = $state('pack');
  let keep = $state(new Set());
  // Mods on the server's own versions go to the pack's too.
  let resetVersions = $state(false);
  let openVersions = $state(false);
  let section = $state(null);
  let filter = $state(0);
  let query = $state('');
  let folders = $state(new Set());
  let openConflicts = $state(false);
  let diffs = $state({});
  let applying = $state(null);
  let result = $state(null);
  let peek = $state(null);

  /** Stops the comparison running, if any (a pending start is a Symbol). */
  function cancelJob() {
    if (jobId != null && typeof jobId !== 'symbol') post(`/jobs/${jobId}/cancel`).catch(() => {});
  }

  async function prepare() {
    if (why) return;
    cancelJob();
    plan = null;
    planErr = '';
    section = null;
    diffs = {};
    openConflicts = false;
    status = { detail: 'Finding the server pack', progress: null };
    const mine = Symbol();
    jobId = mine;
    let job = null;
    try {
      job = (await post(`${base}/pack/plan`, { file: fileId, compare })).job;
      if (jobId !== mine) return post(`/jobs/${job}/cancel`).catch(() => {});
      jobId = job;
      const r = await followJob(job, (st) => jobId === job && (status = st));
      if (jobId !== job) return;
      plan = r;
      defaults(r);
    } catch (e) {
      // Another comparison took over (or the window closed): not this one's error.
      if (jobId !== mine && jobId !== job) return;
      planErr = e.message;
    }
    jobId = null;
    status = null;
  }

  function setCompare(on) {
    compare = on;
    prepare();
  }

  onMount(() => {
    loadLog();
    prepare();
  });

  function close() {
    if (applying) return;
    cancelJob();
    jobId = null;
    onClose();
  }
  onDestroy(() => {
    cancelJob();
  });

  // Escape closes the mod page beside it first, then the window (not while
  // it updates, nor under a confirm dialog or the screenshot viewer).
  function onKey(e) {
    if (e.key !== 'Escape' || e.defaultPrevented || dialog.open) return;
    e.preventDefault();
    if (peek) peek = null;
    else close();
  }

  // ---- the plan ----
  const heldVersions = $derived(plan?.kept_versions || []);
  const mods = $derived([...(plan?.mods || []), ...(resetVersions ? heldVersions : [])]);
  /** "6.2.4" from "GeOre-1.21.1-6.2.4.jar" (the part after the shared name). */
  function versionOf(from, to) {
    const a = from.replace(/\.disabled$/i, '').replace(/\.jar$/i, '');
    const b = to.replace(/\.jar$/i, '');
    let i = 0;
    while (i < a.length && i < b.length && a[i] === b[i]) i++;
    while (i > 0 && !/[-_ ]/.test(a[i - 1])) i--;
    return [a.slice(i), b.slice(i)];
  }
  const versionsNote = $derived.by(() => {
    const some = heldVersions.slice(0, 2).map((m) => { const [y, p] = versionOf(m.from, m.to); return `${m.name} ${y} (pack ${p})`; }).join(', ');
    const more = heldVersions.length > 2 ? '…' : '';
    return resetVersions
      ? `They change to the pack's versions: ${some}${more}. The old jars go to .hopper-backups.`
      : `They stay as they are: ${some}${more}.`;
  });
  const files = $derived(plan?.files || []);
  const conflicts = $derived(files.filter((f) => f.action === 'Conflict').map((f) => f.rel));
  const quick = $derived(plan && !plan.base_known);
  const canCompare = $derived(plan && quick && !plan.compared && !!plan.base);
  const written = $derived(files.filter((f) => ['Add', 'Update', 'Merge'].includes(f.action) || (f.action === 'Conflict' && take.has(f.rel))));
  const changesLabel = $derived(
    plan ? `Changes · ${mods.length} mods · ${written.length} files`
      : planErr ? "Changes · couldn't prepare"
        : why ? 'Changes'
          : `Changes · preparing${status?.progress != null ? ` ${Math.round(status.progress * 100)}%` : '…'}`,
  );

  function defaults(p) {
    take = new Set(p.take || []);
    const n = (p.files || []).filter((f) => f.action === 'Conflict').length;
    bulk = !p.base_known ? 'pack' : take.size === 0 ? 'mine' : take.size === n ? 'pack' : 'suggested';
    keep = new Set();
    resetVersions = false;
    openVersions = false;
  }

  const choices = $derived.by(() => {
    if (!plan) return [];
    const out = [];
    if (quick) {
      out.push(['pack', "Use the pack's"], ['mine', "Keep the server's"]);
    } else {
      const suggested = (plan.take || []).length;
      // "Suggested" only when it differs from both "all" choices.
      if (suggested > 0 && suggested < conflicts.length) out.push(['suggested', 'Suggested']);
      out.push(['mine', "Keep the server's"], ['pack', "Use all the pack's"]);
    }
    out.push(['review', 'Review each']);
    return out;
  });

  function pickBulk(b) {
    if (b === 'suggested') take = new Set(plan.take || []);
    else if (b === 'mine') take = new Set();
    // Without a base, the server's own settings (server.properties…) stay.
    else if (b === 'pack' && !plan.base_known) take = new Set(plan.take || []);
    else if (b === 'pack') take = new Set(conflicts);
    else openConflicts = true;
    bulk = b;
  }

  const mineN = $derived(conflicts.filter((r) => !take.has(r)).length);
  const choiceTitle = $derived.by(() => {
    const n = conflicts.length;
    const f = n === 1 ? 'file' : 'files';
    if (n === 0) return "Compared with the server's changes: none clash with the pack's";
    return quick ? `${n} server ${f} differ from the server pack` : `${n} ${f} you and the pack both changed`;
  });
  const choiceNote = $derived.by(() => {
    const backups = "The server's copies go to .hopper-backups.";
    const n = conflicts.length;
    if (bulk === 'pack' && quick && mineN > 0) return `The new version's copy replaces each one, like the CurseForge app. ${backups} The server's own settings (${conflicts.filter((r) => !take.has(r)).join(', ')}) stay.`;
    if (bulk === 'pack' && quick) return `The new version's copy replaces each one, like the CurseForge app. ${backups}`;
    if (bulk === 'pack') return `The pack's copy replaces every one. ${backups}`;
    if (bulk === 'suggested') return `Pack content (quests, scripts) takes the pack's copy (${n - mineN}); settings keep yours (${mineN}). ${backups}`;
    if (bulk === 'mine') return "The server's copy of every one stays; the pack's changes to them are skipped.";
    return `Picked per file: ${mineN} keep yours, ${n - mineN} take the pack's. ${backups}`;
  });

  // Pressed on Mine / Pack's and dragged down the list: each file takes the same.
  function setTake(rel, theirs) {
    if (take.has(rel) === theirs) return;
    const s = new Set(take);
    theirs ? s.add(rel) : s.delete(rel);
    take = s;
    bulk = 'review';
  }

  const paintAt = (i, v) => conflicts[i] != null && setTake(conflicts[i], v === 1);

  async function toggleDiff(rel) {
    if (diffs[rel] !== undefined) {
      const { [rel]: _, ...rest } = diffs;
      diffs = rest;
      return;
    }
    diffs = { ...diffs, [rel]: 'loading' };
    try {
      const d = (await api(`${base}/pack/diff?plan=${encodeURIComponent(plan.plan)}&rel=${encodeURIComponent(rel)}`)).diff;
      diffs = { ...diffs, [rel]: d ?? 'Not a text file.' };
    } catch (e) {
      diffs = { ...diffs, [rel]: e.message };
    }
  }
  const lineKind = (l) => (l.startsWith('@@') ? 'dh' : l.startsWith('+') && !l.startsWith('+++') ? 'da' : l.startsWith('-') && !l.startsWith('---') ? 'dr' : '');

  function toggleSection(s) {
    section = section === s ? null : s;
    filter = 0;
    query = '';
    folders = new Set();
  }
  const matches = (t) => !query.trim() || (t || '').toLowerCase().includes(query.trim().toLowerCase());

  /** Files grouped by their top folder: [{ folder, rows, counts }]. */
  function tree(rows) {
    const by = new Map();
    for (const r of rows.filter((r) => matches(r.rel))) {
      const i = r.rel.indexOf('/');
      const top = i < 0 ? 'other' : r.rel.slice(0, i + 1);
      if (!by.has(top)) by.set(top, []);
      by.get(top).push(r);
    }
    return [...by.entries()].sort((a, b) => a[0].localeCompare(b[0])).map(([folder, rows]) => {
      const c = new Map();
      for (const r of rows) c.set(r.chip?.[0] || 'files', (c.get(r.chip?.[0] || 'files') || 0) + 1);
      const counts = [...c.entries()].map(([l, n]) => (l === 'files' && n === 1 ? '1 file' : `${n} ${l.toLowerCase()}`)).join(' · ');
      return { folder, rows, counts };
    });
  }
  function toggleFolder(f) {
    const s = new Set(folders);
    s.has(f) ? s.delete(f) : s.add(f);
    folders = s;
  }
  const LIST_CAP = 300;

  // What changes: mods.
  const added = $derived(mods.filter((m) => m.action === 'Add').length);
  const updated = $derived(mods.filter((m) => m.action === 'Update').length);
  const dropped = $derived(mods.filter((m) => m.action === 'Remove').length);
  const needed = $derived(plan?.needed || []);
  const neededFiles = $derived(new Set(needed.map((n) => n.file.toLowerCase())));
  const modRows = $derived.by(() => {
    const want = [null, 'Add', 'Update', 'Remove'][filter];
    return mods
      .filter((m) => !want || m.action === want)
      .map((m) => {
        const need = m.action === 'Remove' && neededFiles.has(m.from.toLowerCase());
        const kept = need && keep.has(m.from.toLowerCase());
        const chip = { Add: ['Added', 'running'], Update: ['Updated', 'lan'], Remove: kept ? ['Kept', 'starting'] : ['Dropped', 'crashed'] }[m.action];
        const detail = m.action === 'Update' ? `${m.from}  →  ${m.to}` : m.action === 'Add' ? m.to : need ? `⚠ one of yours needs it · ${m.from}` : m.from;
        return { m, chip, detail, need };
      })
      .filter((r) => matches(r.m.name) || matches(r.detail))
      .sort((a, b) => Number(b.need) - Number(a.need) || a.m.name.localeCompare(b.m.name, undefined, { sensitivity: 'base' }));
  });

  // What changes: server pack files.
  const newN = $derived(written.filter((f) => f.action === 'Add').length);
  const mergedN = $derived(written.filter((f) => f.action === 'Merge').length);
  const filesText = $derived(
    !written.length ? 'No server pack files change'
      : !plan?.base_known ? `${written.length} server pack files written: ${written.length - newN} replaced, ${newN} new`
        : mergedN ? `${written.length} server pack files updated (${newN} new, ${mergedN} merged)`
          : `${written.length} server pack files updated (${newN} new)`,
  );
  const fileRows = $derived(
    written
      .filter((f) => (filter === 1 ? f.action !== 'Add' : filter === 2 ? f.action === 'Add' : true))
      .map((f) => ({ rel: f.rel, chip: f.action === 'Add' ? ['New', 'running'] : f.action === 'Merge' ? ['Merged', 'lan'] : ['Replaced', 'acc'], diff: f.action !== 'Add' })),
  );
  const gone = $derived(files.filter((f) => f.action === 'Remove').map((f) => ({ rel: f.rel, chip: ['Removed', 'crashed'], diff: false })));

  // What stays.
  const yourMods = $derived(plan?.your_mods || []);
  const yourFiles = $derived(plan?.your_files || []);
  const stayText = $derived([yourMods.length && `${yourMods.length} mods not in the server pack`, yourFiles.length && `${yourFiles.length} of the server's own files`].filter(Boolean).join(' · '));
  const needBy = $derived(new Map(needed.map((n) => [n.by.toLowerCase(), n.name])));
  const stayMods = $derived(
    yourMods
      .map((y) => ({ m: y, need: needBy.get(y.name.toLowerCase()) }))
      .filter((r) => matches(r.m.name) || matches(r.m.from))
      .sort((a, b) => Number(!!b.need) - Number(!!a.need) || a.m.name.localeCompare(b.m.name, undefined, { sensitivity: 'base' })),
  );
  const edited = $derived(files.filter((f) => ['KeepYours', 'KeepDropped'].includes(f.action) || (f.action === 'Conflict' && !take.has(f.rel))).map((f) => ({ rel: f.rel, chip: ['Yours', ''], diff: true })));

  function toggleKeep(file) {
    const s = new Set(keep);
    const k = file.toLowerCase();
    s.has(k) ? s.delete(k) : s.add(k);
    keep = s;
  }

  function openPeek(project) {
    if (project && project !== '0') peek = project;
  }

  // ---- updating ----
  async function update() {
    planErr = '';
    applying = { detail: 'Starting', progress: null };
    try {
      const { job } = await post(`${base}/pack/apply`, { plan_id: plan.plan, take: [...take], keep: [...keep], reset_versions: resetVersions });
      result = await followJob(job, (st) => (applying = st));
      say(`${server.name} is on ${result.version}. Old files are in backups.`);
      onDone();
    } catch (e) {
      // The plan is used up either way: prepare it again.
      say(e.message, true);
      prepare();
    } finally {
      applying = null;
    }
  }

  const strip = $derived(
    plan
      ? `${bulk === 'mine' ? "Mods update to the server pack's; the server's edited files stay as they are." : bulk === 'review' ? "Mods update to the server pack's; files as you picked them." : plan.base_known ? "Mods and the pack's files update to the server pack's, keeping your edits as picked." : "Pack files are replaced with the server pack's, like the CurseForge app."} The server's own mods and files stay.`
      : planErr ? "The update couldn't be prepared: see Changes."
        : why ? why
          : 'Preparing the update in the background…',
  );
</script>

{#snippet sumRow(Icon, tint, open, onclick, body)}
  <button class="sum" class:click={open != null} class:row-hover={open != null} disabled={open == null} {onclick}>
    <span class="si {tint}"><Icon size={13} /></span>
    <span class="sb">{@render body()}</span>
    {#if open != null}<span class="show">{open ? 'Hide' : 'Show'}{#if open}<ChevronDown size={12} />{:else}<Caret size={12} />{/if}</span>{/if}
  </button>
{/snippet}

{#snippet tools(tabs)}
  <div class="tools">
    {#if tabs.length > 1}<div class="utabs" role="tablist">{#each tabs as t, i (i)}<button role="tab" aria-selected={filter === i} class:tab-sel={filter === i} onclick={() => (filter = i)}>{t}</button>{/each}</div>{/if}
    <span class="sp"></span>
    <div class="input find"><input use:rightClear placeholder="Search" bind:value={query} /></div>
  </div>
{/snippet}

{#snippet modCell(m)}
  {#if m.project && m.project !== '0'}
    <button class="mc" title="Open {m.name}" onclick={() => openPeek(m.project)}>{#if m.icon}<img src={m.icon} alt="" width="20" height="20" loading="lazy" />{:else}<span class="ph"></span>{/if}<span class="ell">{m.name}</span></button>
  {:else}
    <span class="mc">{#if m.icon}<img src={m.icon} alt="" width="20" height="20" loading="lazy" />{:else}<span class="ph"></span>{/if}<span class="ell">{m.name}</span></span>
  {/if}
{/snippet}

{#snippet fileTree(rows)}
  {@const groups = tree(rows)}
  {#if !groups.length}<div class="tiny muted">{query ? 'No match.' : 'Nothing here.'}</div>
  {:else}
    <div class="tree">
      {#each groups as g (g.folder)}
        {@const open = !!query.trim() || folders.has(g.folder)}
        <button class="fold" onclick={() => toggleFolder(g.folder)}>{#if open}<ChevronDown size={12} />{:else}<Caret size={12} />{/if}<b class="ell">{g.folder}</b><span class="sp"></span><span class="tiny muted">{g.counts}</span></button>
        {#if open}
          {#each g.rows.slice(0, LIST_CAP) as r (r.rel)}
            <div class="fr">
              <span class="ell tiny grow" title={r.rel}>{r.rel.startsWith(g.folder) ? r.rel.slice(g.folder.length) : r.rel}</span>
              {#if r.chip?.[0]}<span class="chip {r.chip[1]} xs">{r.chip[0]}</span>{/if}
              {#if r.diff}<button class="btn sm ghost dbtn" onclick={() => toggleDiff(r.rel)}>{diffs[r.rel] !== undefined ? 'Hide changes' : 'Changes'}</button>{/if}
            </div>
            {#if diffs[r.rel] !== undefined}{@render diffBlock(diffs[r.rel])}{/if}
          {/each}
          {#if g.rows.length > LIST_CAP}<div class="tiny muted fr">…{g.rows.length - LIST_CAP} more in {g.folder}: search to narrow</div>{/if}
        {/if}
      {/each}
    </div>
  {/if}
{/snippet}

{#snippet diffBlock(d)}
  {#if d === 'loading'}<div class="tiny muted row diff"><span class="spinner"></span>Loading</div>
  {:else}<pre class="diff">{#each d.split('\n').slice(0, 400) as l, i (i)}<span class={lineKind(l)}>{l.replaceAll('\t', '    ')}{'\n'}</span>{/each}</pre>{/if}
{/snippet}

<svelte:window onkeydown={onKey} />

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && !applying && close()}>
  <div class="pair">
    <div class="box popover col">
      <h3 class="row">
        {#if pack.icon}<img class="pi" src={pack.icon} alt="" width="22" height="22" />{:else}<Package size={16} />{/if}
        <span class="ell">Update {server.name}</span><span class="sp"></span>
        <button class="btn sm ghost icon" title="Close" disabled={!!applying} onclick={close}><X size={14} /></button>
      </h3>
      <div class="small subtle row wrap cap">
        <span class="ell">{plan?.base || `${pack.current.name}'s server pack`}  →  {plan?.server_pack || target?.file_name || target?.name}</span>
        {#if onOtherVersion && !result}<button class="link tiny" disabled={!!applying} onclick={onOtherVersion}>Other version…</button>{/if}
      </div>

      {#if result}
        <div class="scroll col">
          <div class="row"><span class="chip running">Updated</span><b>{server.name} is on {result.version}</b></div>
          <div class="small subtle">{result.summary}</div>
          {#if result.backups}
            <div class="small row wrap">Replaced and removed files are in <span class="mono">{result.backups}</span>
              {#if openFiles}<button class="btn sm" onclick={() => { openFiles(result.backups); onClose(); }}><FolderOpen size={13} />Open</button>{/if}
            </div>
          {/if}
          <div class="tiny muted">Players need the same pack version to join: update their instances too.</div>
        </div>
        <div class="foot one"><button class="btn pri" onclick={onClose}>Close</button></div>
      {:else}
        {#if plan?.loader || plan?.mods_note || (plan?.base_known && plan?.base_assumed)}
          <div class="col notes">
            {#if plan.loader}<div class="small row"><ServerCog size={14} class="bluet" />NeoForge {plan.loader.from} → {plan.loader.to}: installed on the server, run script pointed at it.</div>{/if}
            {#if plan.mods_note}<div class="warn small row"><TriangleAlert size={14} /><span>{plan.mods_note}</span></div>{/if}
            {#if plan.base_known && plan.base_assumed}<div class="tiny muted">No record of the server pack this server came from, so it's compared with {plan.base} (the installed version's).</div>{/if}
          </div>
        {/if}

        <nav class="tabs">
          <button class:tab-sel={tab === 0} onclick={() => (tab = 0)}>What's new</button>
          <button class:tab-sel={tab === 1} onclick={() => (tab = 1)}>{changesLabel}</button>
        </nav>

        <div class="scroll col">
          {#if tab === 0}
            {#if !log}<div class="small muted row"><span class="spinner"></span>Loading the changelog</div>
            {:else if log.error}<div class="tiny muted">The changelog couldn't be loaded: {log.error}</div>
            {:else if !log.entries?.length}<div class="small muted">This pack has no changelog.</div>
            {:else}
              {#if log.advice?.length}
                <div class="advice">
                  <div class="row ah"><TriangleAlert size={14} />From the pack author</div>
                  {#each log.advice as a, i (i)}<div>{a}</div>{/each}
                </div>
              {/if}
              {#each log.entries as e (e.name)}
                <div class="entry">
                  <div class="row"><b class="en">{e.name}</b><span class="chip xs">{e.release}</span><span class="tiny muted">{ago(e.date)}</span></div>
                  <div class="notesbody small"><BrowseRich blocks={e.blocks} empty="No changelog for this version." /></div>
                </div>
              {/each}
            {/if}
            <div class="strip small"><span class="acct"><FileText size={14} /></span><span class="grow">{strip}</span><button class="btn sm ghost" onclick={() => (tab = 1)}>See changes ›</button></div>
          {:else if why}
            <div class="warn small row"><TriangleAlert size={14} /><span>{why}</span></div>
          {:else if planErr}
            <div class="small redt row"><TriangleAlert size={14} /><span>{planErr}</span></div>
            <div><button class="btn sm" onclick={prepare}>Try again</button></div>
          {:else if !plan}
            <div class="col prep">
              <div class="small row"><span class="spinner"></span><span class="ell">{status?.detail || 'Preparing'}</span></div>
              <div class="bar" class:busy={status?.progress == null}><i style:width="{status?.progress == null ? 30 : Math.max(2, status.progress * 100)}%"></i></div>
              <div class="tiny muted">{compare ? "The panel reads the old server pack's file list and compares it with the server's files." : 'The panel downloads the new server pack and compares it with the server.'} Nothing changes yet.</div>
            </div>
          {:else}
            <!-- Your choice: mods on the server's own versions -->
            {#if heldVersions.length}
              <div class="choice col">
                <div class="row"><span class="chip acc xs">Your choice</span><b class="small">{heldVersions.length} mod{heldVersions.length === 1 ? '' : 's'} run the server's versions, not the pack's</b></div>
                <div class="seg"><button class:on={!resetVersions} onclick={() => (resetVersions = false)}>Keep the server's</button><button class:on={resetVersions} onclick={() => (resetVersions = true)}>Use the pack's</button></div>
                <div class="tiny muted">{versionsNote}</div>
                <button class="lh small" onclick={() => (openVersions = !openVersions)}>{#if openVersions}<ChevronDown size={13} />Hide the mods{:else}<Caret size={13} />Show the {heldVersions.length} mods{/if}</button>
                {#if openVersions}
                  <div class="list">
                    {#each heldVersions.slice(0, LIST_CAP) as m (m.from)}
                      <div class="mr row-hover">{@render modCell(m)}<span class="tiny muted ell grow">{m.from} → {m.to}</span><span class="chip xs">Server's</span></div>
                    {/each}
                  </div>
                {/if}
              </div>
            {/if}

            <!-- Your choice -->
            {#if conflicts.length || plan.compared}
              <div class="choice col">
                <div class="row"><span class="chip acc xs">Your choice</span><b class="small">{choiceTitle}</b></div>
                {#if conflicts.length}
                  <div class="seg">{#each choices as [k, label] (k)}<button class:on={bulk === k} onclick={() => pickBulk(k)}>{label}</button>{/each}</div>
                  <div class="tiny muted">{choiceNote}</div>
                  <button class="lh small" onclick={() => (openConflicts = !openConflicts)}>{#if openConflicts}<ChevronDown size={13} />Hide the files{:else}<Caret size={13} />Review {conflicts.length} file{conflicts.length === 1 ? '' : 's'}{/if}</button>
                  {#if openConflicts}
                    <div class="cfl">
                      {#each conflicts as rel, ci (rel)}
                        {@const theirs = take.has(rel)}
                        <div class="cf" role="presentation" onpointerenter={(e) => paintOver(e, 'conflicts', ci, paintAt)}>
                          <div class="row">
                            <span class="ell small grow" title={rel}>{rel}</span>
                            <button class="btn sm ghost" onclick={() => toggleDiff(rel)}>{diffs[rel] !== undefined ? 'Hide changes' : 'Changes'}</button>
                            <div class="seg xs">
                              <button class:on={!theirs} onpointerdown={(e) => paintDown(e, 'conflicts', ci, 0, paintAt)} onclick={() => setTake(rel, false)}>Mine</button>
                              <button class:on={theirs} onpointerdown={(e) => paintDown(e, 'conflicts', ci, 1, paintAt)} onclick={() => setTake(rel, true)}>Pack's</button>
                            </div>
                          </div>
                          {#if diffs[rel] !== undefined}{@render diffBlock(diffs[rel])}{/if}
                        </div>
                      {/each}
                    </div>
                  {/if}
                {/if}
                {#if canCompare}
                  <div class="cmp row">
                    <button class="btn sm" onclick={() => setCompare(true)}><GitCompare size={13} />Compare with my changes</button>
                    <span class="tiny muted grow">Finds the files you edited on the server so you can keep them, and merges edits where the pack changed other lines. Reads the old server pack's file list (no full download).</span>
                  </div>
                {/if}
                {#if plan.compared}<div class="row"><span class="sp"></span><button class="btn sm ghost" onclick={() => setCompare(false)}>Back to the quick update</button></div>{/if}
                {#if plan.compared && !plan.base_known}<div class="tiny warn row"><TriangleAlert size={12} />No record of the server pack this server came from, so your edits can't be told from the pack's.</div>{/if}
              </div>
            {:else if canCompare}
              <div class="cmp row">
                <button class="btn sm" onclick={() => setCompare(true)}><GitCompare size={13} />Compare with my changes</button>
                <span class="tiny muted grow">Finds the files you edited on the server so you can keep them, and merges edits where the pack changed other lines. Reads the old server pack's file list (no full download).</span>
              </div>
            {/if}

            <!-- What changes -->
            <div class="col g0">
              <div class="h">What changes</div>
              {#snippet modsBody()}
                {#if !mods.length}<span class="muted">No mod changes</span>
                {:else}
                  {mods.length} mods:
                  {#if added}<span class="chip running xs">{added} added</span>{/if}
                  {#if updated}<span class="chip lan xs">{updated} updated</span>{/if}
                  {#if dropped}<span class="chip crashed xs">{dropped} dropped</span>{/if}
                  {#if needed.length}<span class="chip starting xs">⚠ {needed.length} needed by yours</span>{/if}
                {/if}
              {/snippet}
              {@render sumRow(Package, 'blue', mods.length ? section === 'mods' : null, () => toggleSection('mods'), modsBody)}
              {#if section === 'mods'}
                <div class="area">
                  {#each needed as n (n.file)}
                    {@const kept = keep.has(n.file.toLowerCase())}
                    <div class="need col">
                      <div class="row nh"><TriangleAlert size={14} />The pack drops a mod one of yours needs</div>
                      <div class="row wrap small">{@render modCell({ name: n.by, icon: n.by_icon, project: n.by_project })} needs {@render modCell({ name: n.name, icon: n.icon, project: n.project })}.</div>
                      <div class="small">The new server pack no longer includes it. Without it the server won't start.</div>
                      <div class="row"><button class="btn sm" onclick={() => toggleKeep(n.file)}>{kept ? 'Remove it with the pack' : `Keep ${n.name}`}</button><span class="tiny muted">{kept ? 'Kept: it counts as one of the server\'s own mods from now on, so later updates leave it alone.' : 'Removed with the pack unless you keep it.'}</span></div>
                    </div>
                  {/each}
                  {@render tools([`All ${mods.length}`, `Added ${added}`, `Updated ${updated}`, `Dropped ${dropped}`])}
                  {#if !modRows.length}<div class="tiny muted">{query ? 'No match.' : 'Nothing here.'}</div>
                  {:else}
                    <div class="list">
                      {#each modRows.slice(0, LIST_CAP) as r (r.m.name + r.m.from + r.m.to)}
                        <div class="mr row-hover">{@render modCell(r.m)}<span class="tiny muted ell grow">{r.detail}</span><span class="chip {r.chip[1]} xs">{r.chip[0]}</span></div>
                      {/each}
                      {#if modRows.length > LIST_CAP}<div class="tiny muted mr">…{modRows.length - LIST_CAP} more: search to narrow</div>{/if}
                    </div>
                  {/if}
                  {#if dropped}<div class="tiny muted">Dropped mods go to .hopper-backups.</div>{/if}
                </div>
              {/if}

              {#snippet filesBody()}<span class:muted={!written.length}>{filesText}</span>{/snippet}
              {@render sumRow(FileText, 'acc', written.length ? section === 'files' : null, () => toggleSection('files'), filesBody)}
              {#if section === 'files'}
                <div class="area">
                  {@render tools(['All', `Replaced ${written.length - newN}`, `New ${newN}`])}
                  {@render fileTree(fileRows)}
                  <div class="tiny muted">"Changes" shows the server's copy against the pack's.</div>
                </div>
              {/if}

              {#if gone.length}
                {#snippet goneBody()}{gone.length} files the pack dropped, unedited: to backups{/snippet}
                {@render sumRow(Archive, 'red', section === 'dropped', () => toggleSection('dropped'), goneBody)}
                {#if section === 'dropped'}<div class="area">{@render tools([])}{@render fileTree(gone)}</div>{/if}
              {/if}
            </div>

            <!-- What stays -->
            <div class="col g0">
              <div class="h">What stays</div>
              {#snippet stayBody()}<span class:muted={!stayText}>{stayText || "Nothing of the server's own in the pack's folders"}</span>{/snippet}
              {@render sumRow(Check, 'green', stayText ? section === 'stays' : null, () => toggleSection('stays'), stayBody)}
              {#if section === 'stays'}
                <div class="area">
                  {@render tools([`Its mods ${yourMods.length}`, `Its files ${yourFiles.length}`])}
                  {#if filter === 0}
                    {#if !stayMods.length}<div class="tiny muted">{query ? 'No match.' : 'Nothing here.'}</div>
                    {:else}
                      <div class="list">
                        {#each stayMods.slice(0, LIST_CAP) as r (r.m.name + r.m.from)}
                          <div class="mr row-hover">{@render modCell(r.m)}<span class="tiny muted ell grow">{r.need ? `⚠ needs ${r.need}, which the pack drops` : r.m.from}</span><span class="chip xs" class:starting={r.need}>Yours</span></div>
                        {/each}
                      </div>
                    {/if}
                  {:else}
                    {@render fileTree(yourFiles.map((rel) => ({ rel, chip: null, diff: false })))}
                    <div class="tiny muted">Scripts, packs and the settings of mods added on top: the pack doesn't ship them, so the update writes next to them, never over them.</div>
                  {/if}
                </div>
              {/if}
              {#if plan.kept?.left_off}<div class="tiny muted indent">{plan.kept.left_off} pack mods the server doesn't have stay off.</div>{/if}
              {#if edited.length}
                {#snippet editedBody()}{edited.length} pack files the server changed: kept{/snippet}
                {@render sumRow(Check, 'green', section === 'edited', () => toggleSection('edited'), editedBody)}
                {#if section === 'edited'}<div class="area">{@render tools([])}{@render fileTree(edited)}</div>{/if}
              {/if}
              {#if plan.mod_made}
                {#snippet madeBody()}<span class="muted">{plan.mod_made} settings files the pack's mods made: never touched</span>{/snippet}
                {@render sumRow(Settings2, 'mut', null, null, madeBody)}
              {/if}
              {#snippet neverBody()}<span class="muted">The world, ops, whitelist and bans: never touched</span>{/snippet}
              {@render sumRow(ShieldCheck, 'mut', null, null, neverBody)}
            </div>
            <div class="tiny muted aside">Server packs often leave out global datapacks (Paxi, OpenLoader) and configs of mods new to the pack.</div>
          {/if}
        </div>

        {#if applying}
          <div class="col prep">
            <div class="small gold row"><span class="spinner"></span>{applying.detail || 'Updating'}</div>
            <div class="bar" class:busy={applying.progress == null}><i style:width="{applying.progress == null ? 30 : Math.max(2, applying.progress * 100)}%"></i></div>
          </div>
        {/if}
        {#if plan && alive}<div class="small gold">Stop the server first.</div>{/if}
        <div class="foot">
          <button class="btn" disabled={!!applying} onclick={close}>Cancel</button>
          <button class="btn pri" disabled={!plan || alive || !!applying} title={alive ? 'Stop the server first' : ''} onclick={update}>
            <Download size={14} />{applying ? 'Updating…' : plan ? 'Update' : why ? 'Update' : 'Update · preparing'}
          </button>
        </div>
      {/if}
    </div>

    {#if peek}
      <div class="peek popover">
        {#key peek}<BrowsePack source="curseforge" id={peek} type="mods" onBack={() => (peek = null)} />{/key}
      </div>
    {/if}
  </div>
</div>

<style>
  .pair { display: flex; gap: 14px; align-items: flex-start; max-width: 100%; }
  .box { width: min(780px, calc(100vw - 32px)); height: calc(100dvh - var(--dialog-top) - 24px); gap: 10px; padding: 14px; }
  .peek { width: 460px; flex: none; height: calc(100dvh - var(--dialog-top) - 24px); overflow: auto; }
  @media (max-width: 1300px) { .peek { position: fixed; right: 16px; top: var(--dialog-top); z-index: 61; } }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .pi { border-radius: 5px; background: var(--raise); flex: none; }
  .cap { gap: 10px; }
  .link { color: var(--acc); text-decoration: underline; }
  .link:disabled { opacity: .5; }
  .notes { gap: 4px; }
  .tabs { display: flex; gap: 20px; border-bottom: 1px solid var(--line); flex: none; }
  .tabs button { padding: 7px 0; color: var(--subtle); font-weight: 500; }
  .scroll { overflow: auto; min-height: 0; flex: 1; gap: 16px; padding-right: 4px; }
  .grow { flex: 1; min-width: 0; }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .advice { border-left: 2px solid var(--gold); background: #eab3080f; border-radius: 0 6px 6px 0; padding: 8px 10px 8px 12px; display: flex; flex-direction: column; gap: 3px; font-size: 13px; }
  .ah { color: var(--gold); font-weight: 600; gap: 6px; }
  .entry { display: flex; flex-direction: column; gap: 6px; }
  .en { font-size: 15px; }
  .notesbody { overflow-wrap: anywhere; }
  .strip { display: flex; align-items: center; gap: 10px; background: var(--raise); border-radius: 8px; padding: 8px 10px; position: sticky; bottom: 0; }
  .acct { color: var(--acc); flex: none; display: grid; }
  .choice { border: 1px solid #f9731655; background: #f973160a; border-radius: 8px; padding: 10px 12px; gap: 8px; }
  .seg { display: inline-flex; flex-wrap: wrap; background: var(--base); border-radius: 7px; padding: 2px; gap: 2px; align-self: flex-start; flex: none; }
  .seg button { padding: 3px 10px; font-size: 12.5px; color: var(--subtle); border-radius: 5px; }
  .seg button.on { background: var(--raise); color: var(--text); box-shadow: 0 0 0 1px var(--line2); }
  .seg.xs button { padding: 1px 8px; font-size: 11.5px; }
  .lh { display: flex; align-items: center; gap: 4px; color: var(--subtle); align-self: flex-start; }
  .lh:hover { color: var(--acc); }
  .cfl { display: flex; flex-direction: column; padding-left: 20px; gap: 1px; max-height: 340px; overflow: auto; user-select: none; }
  .cf { padding: 2px 0; }
  .cmp { border: 1px dashed var(--line2); border-radius: 8px; padding: 8px 10px; align-items: flex-start; gap: 12px; }
  .g0 { gap: 2px; }
  .h { font-weight: 600; font-size: 13.5px; margin-bottom: 2px; }
  .sum { display: flex; align-items: center; gap: 10px; text-align: left; padding: 4px 6px; border-radius: 6px; width: 100%; cursor: default; }
  .sum.click { cursor: pointer; }
  .sum:disabled { opacity: 1; }
  .si { width: 20px; height: 20px; border-radius: 5px; display: grid; place-items: center; flex: none; }
  .si.blue { background: var(--blue-dim); color: var(--blue); } .si.acc { background: var(--acc-dim); color: var(--acc); }
  .si.red { background: var(--red-dim); color: var(--red); } .si.green { background: var(--green-dim); color: var(--green); }
  .si.mut { background: #ffffff10; color: var(--muted); }
  .sb { flex: 1; min-width: 0; display: flex; align-items: center; gap: 6px; flex-wrap: wrap; font-size: 13px; }
  .show { display: inline-flex; align-items: center; gap: 2px; color: var(--muted); font-size: 12px; }
  .sum.click:hover .show { color: var(--acc); }
  .area { display: flex; flex-direction: column; gap: 8px; margin: 4px 0 8px 30px; padding: 10px; background: var(--base); border: 1px solid var(--line); border-radius: 8px; }
  .tools { display: flex; align-items: center; gap: 8px; }
  .find { padding: 3px 8px; width: 200px; }
  .list, .tree { border: 1px solid var(--line); border-radius: 6px; display: flex; flex-direction: column; max-height: 360px; overflow: auto; }
  .mr { display: flex; align-items: center; gap: 12px; padding: 3px 8px; border-bottom: 1px solid #ffffff08; }
  .mc { display: inline-flex; align-items: center; gap: 8px; width: 220px; flex: none; min-width: 0; font-size: 12.5px; text-align: left; }
  button.mc:hover { color: var(--acc); }
  .need .mc { width: auto; }
  .mc img, .ph { width: 20px; height: 20px; border-radius: 4px; background: var(--raise); flex: none; }
  .fold { display: flex; align-items: center; gap: 6px; padding: 4px 8px; border-bottom: 1px solid var(--line); text-align: left; font-size: 12.5px; }
  .fold:hover { color: var(--acc); }
  .fr { display: flex; align-items: center; gap: 8px; padding: 1px 8px 1px 26px; border-bottom: 1px solid #ffffff08; min-height: 24px; }
  .dbtn { width: 98px; justify-content: flex-end; flex: none; }
  .chip.xs { font-size: 10.5px; padding: 0 6px; }
  .need { border: 1px solid #eab30866; background: #eab3080d; border-radius: 8px; padding: 8px 10px; gap: 6px; }
  .nh { color: var(--gold); font-weight: 600; font-size: 13px; gap: 6px; }
  .indent { padding-left: 36px; }
  .aside { border-left: 2px solid var(--line2); padding-left: 10px; }
  .diff { margin: 4px 8px; max-height: 260px; overflow: auto; background: var(--term); border: 1px solid var(--line); border-radius: 6px; padding: 6px 8px; font: 11.5px/1.5 var(--mono); white-space: pre; color: var(--muted); }
  .diff .da { color: var(--green); } .diff .dr { color: var(--red); } .diff .dh { color: var(--blue); }
  .warn { color: var(--gold); gap: 6px; align-items: flex-start; }
  .redt { color: var(--red); gap: 6px; align-items: flex-start; }
  .gold { color: var(--gold); }
  .prep { gap: 8px; }
  .foot { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; flex: none; }
  .foot.one { grid-template-columns: 1fr; }
  .foot .btn { justify-content: center; padding: 7px; }
  .icon { padding: 3px 5px; }
  :global(.bluet) { color: var(--blue); }
</style>
