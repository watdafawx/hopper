<script>
  // The Mods tab, laid out like Hopper desktop's: a table with Update /
  // What changed (or Changelog) per row, Author, Side, On and Added
  // columns that sort, checkboxes for bulk work (the bar floats at the
  // bottom), a ⋯ / right-click menu, and the side panel a mod's name or
  // icon opens. The keys work as in desktop: ↑ ↓ Home End PgUp PgDn move,
  // Space turns on or off, Enter opens. Unknown jars are identified in the
  // background and updates are checked on CurseForge and Modrinth.
  import { untrack } from 'svelte';
  import { browseHash } from './lib/browse.js';
  import CfKey from './CfKey.svelte';
  import ModPeek from './ModPeek.svelte';
  import ModVersions from './ModVersions.svelte';
  import ReviewUpdates from './ReviewUpdates.svelte';
  import ModBackups from './ModBackups.svelte';
  import ModProfiles from './ModProfiles.svelte';
  import ModsConfigPick from './ModsConfigPick.svelte';
  import LoaderVersions from './LoaderVersions.svelte';
  import {
    Search, Plus, ScanSearch, RefreshCw, X, Settings2, Lock, LockOpen, Download, ScrollText, Ellipsis, Trash2,
    Monitor, Server as ServerIcon, ArrowLeftRight, ArrowDown, ArrowUp, ExternalLink, Shuffle, FolderOpen, CircleCheck, Clock,
    CircleOff, TriangleAlert, ArrowRight, ChevronDown, Pencil, Package,
  } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { ask } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';
  import {
    setOn, removeMods, problems, setLocked, resetConfig, switchSource, setVersionLock, configFiles, versionLocked, wordsMatch, ago, dateOf,
  } from './lib/modops.js';
  import { pick, startPick, swept, sweeping, clickSwallowed, tickLabel, countLabel } from './lib/dragpick.svelte.js';
  import { rightClear } from './lib/rightclear.js';

  let { server } = $props();
  // The live feed hands over a new `server` object every second; effects
  // that should run once per server key on the id alone.
  const sid = $derived(server.id);
  let sub = $state('mods');
  let data = $state(null);
  let filter = $state('');
  let chip = $state('all');
  let busy = $state('');
  let identifying = $state('');
  let updates = $state(null);
  let checking = $state(false);
  // The review's updates (all pending, or the selected rows'), or null.
  let reviewing = $state(null);
  // The side panel's mod: its jar, and its project so it stays open when
  // an update or a version change renames the jar.
  let picked = $state(null);
  let versionsFor = $state(null);
  let cfgPick = $state(null);
  let loaderOpen = $state(false);
  let backupCount = $state(null);
  let selected = $state(new Set());
  // The last row clicked (Shift+click picks from it to the next one).
  let anchor = null;
  let menu = $state(null);
  let updating = $state(new Set());
  let done = $state(new Set());
  // The keys' row, shown with a ring while the table has focus.
  let cursor = $state(-1);
  let keys = $state(false);
  let focused = $state(false);
  let catTries = 0;

  const pref = (k, d) => {
    try {
      const v = localStorage.getItem(`hopperd-mods-${k}`);
      return v == null ? d : JSON.parse(v);
    } catch {
      return d;
    }
  };
  const keep = (k, v) => {
    try {
      localStorage.setItem(`hopperd-mods-${k}`, JSON.stringify(v));
    } catch {
      // private mode: not remembered
    }
  };
  let onlyYours = $state(pref('yours', false));
  let updatesFirst = $state(pref('updates-first', true));
  // Desktop's default: newest added first.
  let sort = $state(pref('sort', { col: 'added', desc: true }));
  $effect(() => keep('yours', onlyYours));
  $effect(() => keep('updates-first', updatesFirst));
  $effect(() => keep('sort', sort));

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const mods = $derived(data?.mods || []);
  // "Yours" only means something on a server made from a pack.
  const hasPack = $derived(!!data?.pack);
  const yours = (m) => hasPack && m.source && !m.from_pack;
  const pending = (m) => !!m.update && !m.locked && !done.has(m.project_id);
  const base0 = $derived(mods.filter((m) => !onlyYours || !hasPack || yours(m)));
  const counts = $derived({
    all: base0.length,
    updates: base0.filter(pending).length,
    off: base0.filter((m) => !m.enabled).length,
    client: base0.filter((m) => m.side === 'client').length,
    unknown: base0.filter((m) => !m.source).length,
    yours: mods.filter(yours).length,
  });
  // Desktop's All / Updates / Off, then the server's own two when any.
  const CHIPS = [['all', 'All'], ['updates', 'Updates'], ['off', 'Off'], ['client', 'Client-only'], ['unknown', 'Unknown']];
  const fits = {
    all: () => true, updates: pending, off: (m) => !m.enabled,
    client: (m) => m.side === 'client', unknown: (m) => !m.source,
  };
  const by = {
    name: (a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()),
    author: (a, b) => (a.author || '').toLowerCase().localeCompare((b.author || '').toLowerCase()),
    side: (a, b) => (a.side || '').localeCompare(b.side || ''),
    on: (a, b) => Number(a.enabled) - Number(b.enabled),
    added: (a, b) => (a.date_added || '').localeCompare(b.date_added || ''),
  };
  const shown = $derived.by(() => {
    const q = filter.trim();
    const rows = base0.filter((m) => fits[chip](m) && (!q || wordsMatch(q, [m.name, m.file, m.author])));
    const cmp = by[sort.col] || by.name;
    rows.sort((a, b) => {
      if (updatesFirst) {
        const d = Number(pending(b)) - Number(pending(a));
        if (d) return d;
      }
      const c = cmp(a, b);
      return (sort.desc ? -c : c) || by.name(a, b);
    });
    return rows;
  });
  // No mod says where it runs: no Side column.
  const hasSide = $derived(mods.some((m) => m.side));
  // The same mod in twice (an old jar left beside the new one): every
  // copy but the newest, as desktop's "Keep newest" finds them.
  const twice = $derived.by(() => {
    const g = new Map();
    for (const m of mods) if (m.source && m.project_id !== '0') g.set(m.project_id, [...(g.get(m.project_id) || []), m]);
    return [...g.values()].filter((l) => l.length > 1);
  });
  const older = $derived(twice.flatMap((l) => {
    const keep = [...l].sort((a, b) => Number(b.enabled) - Number(a.enabled) || (b.file_date || '').localeCompare(a.file_date || ''))[0];
    return l.filter((m) => m !== keep);
  }));
  const clientOn = $derived(mods.filter((m) => m.side === 'client' && m.enabled).length);
  const pickedMod = $derived(picked ? mods.find((m) => m.file === picked.file) || (picked.pid !== '0' && mods.find((m) => m.project_id === picked.pid)) || null : null);
  const peekOf = (m) => ({ file: m.file, pid: m.project_id || '0' });
  // Picks of mods still here (a remove elsewhere drops theirs).
  const selMods = $derived(mods.filter((m) => selected.has(m.file)));
  const selUpdates = $derived(selMods.filter(pending));
  // Lock unless every picked mod is locked already.
  const allLocked = $derived(selMods.filter((m) => m.source).length > 0 && selMods.filter((m) => m.source).every((m) => m.locked));
  const allShownPicked = $derived(shown.length > 0 && shown.every((m) => selected.has(m.file)));

  // Big packs have 1000+ mods: only the rows near the screen are drawn
  // (every row is ROW px tall), the rest is empty space of the same height.
  const ROW = 52;
  const EXTRA = 12;
  let rowsEl = $state();
  let win = $state({ a: 0, b: 40 });
  const visible = $derived(shown.slice(win.a, win.b));
  function measure() {
    if (!rowsEl) return;
    const sc = rowsEl.closest('.scroll');
    const view = sc ? sc.getBoundingClientRect() : { top: 0, height: innerHeight };
    const top = rowsEl.getBoundingClientRect().top - view.top;
    const a = Math.max(0, Math.floor(-top / ROW) - EXTRA);
    const b = Math.max(a + 1, Math.ceil((view.height - top) / ROW) + EXTRA);
    if (a !== win.a || b !== win.b) win = { a, b };
  }
  $effect(() => {
    if (!rowsEl) return;
    const sc = rowsEl.closest('.scroll') || window;
    const on = () => measure();
    sc.addEventListener('scroll', on, { passive: true });
    window.addEventListener('resize', on);
    // measure() reads the window it sets: not a reason to run this again.
    untrack(measure);
    return () => {
      sc.removeEventListener('scroll', on);
      window.removeEventListener('resize', on);
    };
  });
  // The list's length or the banners above it changed: the window moves.
  $effect(() => {
    shown.length;
    [busy, identifying, twice.length, clientOn, sub, data?.curseforge];
    queueMicrotask(measure);
  });
  // The offers on the rows: the table, the review and the counts agree.
  // One per mod: a mod in twice has an offer on each copy.
  const offers = (list) => [...new Map(list.filter(pending).map((m) => [m.project_id, m.update])).values()];
  const pendingUpdates = $derived(updates ? offers(mods) : []);

  async function load() {
    const b = base;
    try {
      const d = await api(`${b}/mods/list`);
      // Another server opened meanwhile: not its list.
      if (b !== base) return;
      data = d;
      // Categories arrive from the sites in the background: look again.
      if (d.categories_pending && catTries++ < 6) setTimeout(() => b === base && load(), 4000);
    } catch (e) {
      if (b === base) say(e.message, true);
    }
  }

  async function loadBackupCount() {
    try {
      backupCount = (await api(`${base}/mod-backups`)).sets.length;
    } catch {
      backupCount = null;
    }
  }

  async function run(label, path, body = {}) {
    try {
      const { job } = await post(`${base}/mods/${path}`, body);
      busy = label;
      return await followJob(job, (st) => (busy = st.detail || label));
    } catch (e) {
      say(e.message, true);
      return null;
    } finally {
      busy = '';
      await load();
    }
  }

  /** Identifies unknown jars; on its own once per server per visit. */
  async function identify(auto = false) {
    if (identifying) return;
    identifying = 'Identifying jars on CurseForge and Modrinth';
    try {
      const { job } = await post(`${base}/mods/identify`);
      const r = await followJob(job, (st) => (identifying = st.detail || identifying));
      const moved = r?.moved_to_curseforge ? `; ${r.moved_to_curseforge} now from CurseForge, like the pack` : '';
      if (!auto || r?.identified || r?.moved_to_curseforge) say((r.unknown ? `Identified ${r.identified}; ${r.unknown} still unknown` : `Identified ${r.identified}`) + moved);
      await load();
      checkUpdates(false);
    } catch (e) {
      if (!auto) say(e.message, true);
    } finally {
      identifying = '';
    }
  }

  async function checkUpdates(force) {
    checking = true;
    try {
      const r = await api(`${base}/mods/updates${force ? '?force=1' : ''}`);
      updates = r.updates;
      await load();
      if (force && !updates.length) say(hasPack ? 'Every mod added on top of the pack is up to date.' : 'Every mod is up to date.');
    } catch (e) {
      if (force) say(e.message, true);
    } finally {
      checking = false;
    }
  }

  async function applyUpdates(ids) {
    updating = new Set([...updating, ...ids]);
    const r = await run(ids.length === 1 ? 'Updating a server mod' : `Updating ${ids.length} server mods`, 'update', { project_ids: ids });
    updating = new Set([...updating].filter((i) => !ids.includes(i)));
    if (r) {
      const n = r.updated.length;
      const bad = r.failed?.length ? ` ${r.failed.length} failed: ${r.failed.slice(0, 3).join(', ')}${r.failed.length > 3 ? '…' : ''}.` : '';
      const web = r.web ? ` ${r.web} only download from CurseForge's website: get them on its page and drop them in mods/.` : '';
      say(`${n === 1 && ids.length === 1 ? `Updated ${r.updated[0]}` : `Updated ${n}`}.${bad}${web}${n && alive ? ' Restart the server to load them.' : ''}${n ? ' The old jars are in Backups.' : ''}`, !n);
      // The quiet green "Updated" mark, on the rows that did (the job
      // names them as the server knows them).
      const names = new Set(r.updated);
      const ok = ids.filter((i) => mods.some((m) => m.project_id === i && (names.has(m.name) || names.has(m.update?.name))));
      done = new Set([...done, ...ok]);
      reviewing = null;
      // The rest stay listed while the check runs.
      if (updates) updates = updates.filter((u) => !ids.includes(u.project_id));
      checkUpdates(false);
      loadBackupCount();
    }
  }

  async function changed(big) {
    await load();
    if (big) {
      checkUpdates(false);
      loadBackupCount();
    }
  }

  // The switch only moves once the change is made (a question can cancel it).
  async function toggle(m, e) {
    e?.preventDefault();
    e?.stopPropagation();
    if (e && clickSwallowed()) return;
    if (await setOn(base, [m], !m.enabled, alive)) await load();
  }

  // Remove, with Undo in its toast (it reloads the list once they're back).
  const drop = (list) => removeMods(base, list, alive, { onUndo: () => changed(true) });

  async function bulk(kind) {
    const list = selMods;
    let ok = false;
    if (kind === 'on' || kind === 'off') ok = await setOn(base, list, kind === 'on', alive);
    else if (kind === 'lock') ok = await setLocked(base, list.filter((m) => m.source), !allLocked);
    else if (kind === 'remove') ok = await drop(list);
    else if (kind === 'update') return (reviewing = offers(selUpdates));
    if (ok) {
      if (kind === 'remove') selected = new Set();
      changed(kind !== 'on' && kind !== 'off');
    }
  }

  /** An update on a row: a locked-out version only with Shift, as in desktop. */
  function updateOne(m, e) {
    if (clickSwallowed()) return;
    if (versionLocked(m) && !e?.shiftKey) return say('This version is locked: Shift+click Update to install it anyway.');
    applyUpdates([m.project_id]);
  }

  async function lockVersion(m, lock) {
    if (await setVersionLock(base, m, lock ? m.update.file : null)) await load();
  }

  async function editConfig(m) {
    const files = await configFiles(base, m);
    if (!files) return;
    if (!files.length) return say(`No config files named after ${m.name} in config/.`);
    if (files.length === 1) return openEditor(files[0]);
    cfgPick = { mod: m, files };
  }

  function openEditor(f) {
    cfgPick = null;
    location.hash = `/server/${[server.id, 'edit', ...f.split('/')].map(encodeURIComponent).join('/')}`;
  }

  // Drag across rows, as in desktop: tick boxes tick as the pointer
  // crosses them; switches and Update buttons act on letting go.
  function tickDown(e, m, ix) {
    if (e.shiftKey) return;
    const on = !selected.has(m.file);
    startPick(e, {
      list: 'mods', kind: 'tick', ix, on, selected,
      keyAt: (i) => shown[i]?.file ?? null,
      onTick: (s) => (selected = s),
      label: tickLabel(on),
    });
  }
  function switchDown(e, m, ix) {
    const on = !m.enabled;
    // Only the switches that change.
    const keyAt = (i) => (shown[i] && shown[i].enabled !== on ? shown[i].file : null);
    startPick(e, {
      list: 'mods', kind: 'switch', ix, on, keyAt,
      label: countLabel(on ? 'Turning on' : 'Turning off', on ? 'All on' : 'All off'),
      onCommit: async (files) => {
        // All at once, so what they need is checked once, for the lot.
        if (await setOn(base, mods.filter((x) => files.includes(x.file)), on, alive)) await load();
      },
    });
  }
  // Drag-updates pass over a locked version and one that stops loading.
  const sweepable = (m) => m && pending(m) && !updating.has(m.project_id) && !versionLocked(m) && !problems(m.update).length;
  function updateDown(e, m, ix) {
    startPick(e, {
      list: 'mods', kind: 'action', ix, on: true,
      keyAt: (i) => (sweepable(shown[i]) ? shown[i].project_id : null),
      label: countLabel('Updating', 'No updates'),
      onCommit: (ids) => applyUpdates(ids),
    });
  }
  const swSweep = $derived(sweeping('mods'));

  /** Rows a to b picked (Shift+click); without `add` (Ctrl) the others go. */
  function selectRange(a, b, add) {
    const s = add ? new Set(selected) : new Set();
    for (const x of shown.slice(Math.min(a, b), Math.max(a, b) + 1)) s.add(x.file);
    selected = s;
  }

  function check(m, ix, e) {
    e.stopPropagation();
    if (clickSwallowed()) return e.preventDefault();
    const box = e.currentTarget;
    const a = anchor == null ? -1 : shown.findIndex((x) => x.file === anchor);
    if (e.shiftKey && a >= 0) selectRange(a, ix, true);
    else {
      const s = new Set(selected);
      s.has(m.file) ? s.delete(m.file) : s.add(m.file);
      selected = s;
    }
    anchor = m.file;
    cursor = ix;
    // Shift+click on a ticked box keeps it ticked: the box flipped itself,
    // and the state didn't change, so nothing would put it back.
    setTimeout(() => (box.checked = selected.has(m.file)));
  }

  // A click puts the keys on the row; Ctrl also picks or unpicks it, Shift
  // picks every row from the last one clicked, like a file explorer.
  function rowClick(m, ix, e) {
    if (clickSwallowed() || e.target.closest('button, input, label, a, .nmlink')) return;
    cursor = ix;
    keys = false;
    rowsEl?.focus({ preventScroll: true });
    const a = anchor == null ? -1 : shown.findIndex((x) => x.file === anchor);
    if (e.shiftKey && a >= 0) selectRange(a, ix, e.ctrlKey || e.metaKey);
    else {
      anchor = m.file;
      if (e.ctrlKey || e.metaKey) {
        const s = new Set(selected);
        s.has(m.file) ? s.delete(m.file) : s.add(m.file);
        selected = s;
      }
    }
  }

  function peek(m, e) {
    e?.stopPropagation();
    picked = picked?.file === m.file ? null : peekOf(m);
  }

  /** Moves the key cursor (clamped) and scrolls its row into view. */
  function moveCursor(ix) {
    if (!shown.length) return;
    cursor = Math.max(0, Math.min(shown.length - 1, ix));
    keys = true;
    const sc = rowsEl?.closest('.scroll');
    if (!sc) return;
    const v = sc.getBoundingClientRect();
    const top = rowsEl.getBoundingClientRect().top - v.top + sc.scrollTop + cursor * ROW;
    // Room for the floating selection bar.
    const room = v.height - (selMods.length ? 70 : 8);
    if (top < sc.scrollTop + 40) sc.scrollTop = top - 40;
    else if (top + ROW > sc.scrollTop + room) sc.scrollTop = top + ROW - room;
  }

  function tableKey(e) {
    if (e.target.closest('input, button, a, label, select')) return;
    const steps = { ArrowUp: -1, ArrowDown: 1, PageUp: -10, PageDown: 10 };
    if (e.key in steps) moveCursor((cursor < 0 ? -1 : cursor) + steps[e.key]);
    else if (e.key === 'Home') moveCursor(0);
    else if (e.key === 'End') moveCursor(shown.length - 1);
    else if (e.key === ' ' && shown[cursor]) toggle(shown[cursor]);
    else if (e.key === 'Enter' && shown[cursor]) picked = peekOf(shown[cursor]);
    else return;
    e.preventDefault();
  }

  function checkAll() {
    selected = allShownPicked ? new Set() : new Set(shown.map((m) => m.file));
  }

  // Dates read newest first; text reads A to Z.
  function setSort(col) {
    sort = sort.col === col ? { col, desc: !sort.desc } : { col, desc: col === 'added' };
  }

  function clearFilters() {
    filter = '';
    chip = 'all';
    onlyYours = false;
  }

  // The ⋯ button opens it under itself; right-click at the pointer.
  function openMenu(m, e) {
    e.preventDefault();
    e.stopPropagation();
    const w = 250;
    const h = 400;
    const r = e.type === 'click' ? e.currentTarget.getBoundingClientRect() : null;
    const x = r ? r.right - w : e.clientX;
    const y = r ? r.bottom + 4 : e.clientY;
    menu = { m, x: Math.max(8, Math.min(x, innerWidth - w - 8)), y: Math.max(8, Math.min(y, innerHeight - h - 8)) };
  }

  // Escape: a menu closes first; then the picks go (not while a dialog is up).
  function escape(e) {
    if (e.key !== 'Escape') return;
    if (menu) menu = null;
    else if (!versionsFor && !reviewing && !cfgPick && !loaderOpen && !document.querySelector('.dialog-back') && !e.target.closest?.('input, textarea')) selected = new Set();
  }

  async function act(f) {
    const m = menu.m;
    menu = null;
    await f(m);
  }

  async function editMeta() {
    const v = await ask('Minecraft version and loader', `${data.game_version} ${data.loader}`.trim(), {
      body: 'Used to pick mod files. For example: 1.21.1 neoforge-21.1.259 (or fabric, quilt, forge-47.4.0).',
      ok: 'Save',
    });
    if (!v) return;
    const [game_version, loader = ''] = v.trim().split(/\s+/);
    try {
      await api(`${base}/mods/meta`, { method: 'PUT', body: { game_version, loader } });
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function first() {
    picked = null;
    updates = null;
    data = null;
    selected = new Set();
    done = new Set();
    cursor = -1;
    catTries = 0;
    await load();
    loadBackupCount();
    const key = `hopperd-identified-${sid}`;
    let did = false;
    try {
      did = sessionStorage.getItem(key) === '1';
      sessionStorage.setItem(key, '1');
    } catch {
      // private mode: identify every visit
    }
    if (counts.unknown && !did) identify(true);
    else checkUpdates(false);
  }

  // Once per server: first() reads state, which must not
  // make this run again.
  $effect(() => {
    sid;
    untrack(first);
  });

  const SIDE = { client: [Monitor, 'Client only: players need it, the server usually doesn\'t'], server: [ServerIcon, 'Server only'], both: [ArrowLeftRight, 'Client and server: both need it'] };
  // Desktop's tags: Client only (gold), Server only (blue), Client + server.
  const SIDE_TAG = { client: ['Client only', 'warnc'], server: ['Server only', 'info'], both: ['Client + server', ''] };
  const SIDE_HEAD = 'Where the mod runs:\nClient only: players need it\nServer only\nClient and server: both need it\nHover a row\'s icon for more. Click to sort.';
  const site = (m) => (m.source === 'modrinth' ? 'Modrinth' : 'CurseForge');
  const isNew = (m) => !!m.date_added && Date.now() - new Date(m.date_added).getTime() < 3 * 86400000;
  const browseOf = (m) => (m.source === 'modrinth' ? m.mr_project : m.source ? m.project_id : '');
  // A mod named in an update's "breaks X", for its icon and side panel.
  const byName = (n) => mods.find((m) => m.name === n) || null;
  const nameTip = (m) => [m.author ? `by ${m.author}` : '', 'Details'].filter(Boolean).join('\n');
</script>

<svelte:window onclick={() => (menu = null)} oncontextmenu={() => (menu = null)} onkeydown={escape} />

{#snippet sortHead(col, label, cls, tip)}
  <button class={cls} class:act={sort.col === col} title={tip} onclick={() => setSort(col)}>{label}{#if sort.col === col}{#if sort.desc}<ArrowDown size={11} />{:else}<ArrowUp size={11} />{/if}{/if}</button>
{/snippet}

<div class="card">
  <nav class="subtabs">
    <button class="tab" class:tab-sel={sub === 'mods'} onclick={() => (sub = 'mods')}>Mods{data ? ` ${mods.length}` : ''}</button>
    <button class="tab" class:tab-sel={sub === 'backups'} onclick={() => (sub = 'backups')}>Backups{backupCount ? ` ${backupCount}` : ''}</button>
    <span class="sp"></span>
    {#if data}
      <button class="chip meta" title={/^(neoforge|forge|fabric|quilt)/i.test(data.loader || '') ? 'Loader versions and changelog' : 'Minecraft version and loader: change'} onclick={() => (/^(neoforge|forge|fabric|quilt)/i.test(data.loader || '') ? (loaderOpen = true) : editMeta())}><Settings2 size={11} />{data.game_version || 'version?'} · {data.loader || 'loader?'}</button>
      {#if data.pack}<span class="chip acc" title="Pack mods update through Update from server pack">{data.pack}</span>{/if}
    {/if}
  </nav>

  {#if sub === 'backups'}
    <ModBackups {server} onChanged={() => { load(); loadBackupCount(); checkUpdates(false); }} />
  {:else}
    <div class="row wrap toolbar">
      <div class="input find"><Search size={13} /><input use:rightClear placeholder="Search" bind:value={filter} />{#if filter}<button class="clr" title="Clear" onclick={() => (filter = '')}><X size={12} /></button>{/if}</div>
      <a class="btn sm" href="#{browseHash('mods', server.id)}"><Plus size={13} />Add content</a>
      <ModProfiles {base} {alive} onChanged={() => changed(false)} />
      <div class="utabs" role="tablist">
        {#each CHIPS as [k, label] (k)}
          {#if k === 'all' || k === 'updates' || k === 'off' || counts[k] || chip === k}<button role="tab" aria-selected={chip === k} class:tab-sel={chip === k} onclick={() => (chip = k)}>{label}{k === 'all' ? '' : ` ${counts[k]}`}</button>{/if}
        {/each}
      </div>
      {#if hasPack}<label class="cbl" title="Only what you added on top of the modpack"><input type="checkbox" bind:checked={onlyYours} />Only yours ({counts.yours})</label>{/if}
      <label class="cbl"><input type="checkbox" bind:checked={updatesFirst} />Updates first</label>
      <span class="sp"></span>
      {#if alive}<span class="tiny muted">The server is running: changes apply on its next start</span>{/if}
      {#if counts.unknown}<button class="btn sm ghost" disabled={!!identifying} onclick={() => identify(false)} title="Look jars up on CurseForge and Modrinth"><ScanSearch size={13} />Identify {counts.unknown}</button>{/if}
      <span class="row upd">
        <!-- The last check's results stay up while a new one runs; with results on screen the refresh button spins instead, so nothing moves. -->
        {#if checking && !pendingUpdates.length}<span class="small muted row"><span class="spinner blue"></span>Checking for updates</span>{/if}
        {#if pendingUpdates.length}<button class="btn sm pri" disabled={!!busy} onclick={() => (reviewing = pendingUpdates)}><Download size={13} />Review {pendingUpdates.length} update{pendingUpdates.length === 1 ? '' : 's'}</button>
        {:else if updates && !checking}<span class="small okt row"><CircleCheck size={14} />Up to date</span>{/if}
        <button class="btn sm ghost icon" disabled={checking} title={checking ? 'Checking for updates' : 'Check for updates again'} onclick={() => checkUpdates(true)}>{#if checking && pendingUpdates.length}<span class="spinner blue"></span>{:else}<RefreshCw size={13} />{/if}</button>
      </span>
    </div>
    {#if identifying}<div class="banner small"><span class="spinner blue"></span>{identifying}… names, icons and updates fill in when it's done.</div>{/if}
    {#if busy}<div class="busy small"><span class="spinner"></span>{busy}</div>{/if}
    {#if data && !data.curseforge}<CfKey inline onSaved={() => { load(); checkUpdates(true); }} />{/if}
    {#if twice.length}<div class="small warn row wrap">{twice.length} mod{twice.length === 1 ? ' is' : 's are'} in twice: {twice.map((l) => l[0].name).join(', ')}. <button class="link" onclick={async () => (await drop(older)) && changed(true)}>Keep newest</button><span class="muted">(the older jars go to Backups)</span></div>{/if}
    {#if clientOn}<div class="small warn">{clientOn} client-only mod{clientOn === 1 ? '' : 's'} on: servers usually don't need {clientOn === 1 ? 'it' : 'them'}, and some won't start with them. <button class="link" onclick={() => (chip = 'client')}>Show</button></div>{/if}

    <div class="split">
      <div class="list" class:noside={!hasSide}>
        <div class="tr th">
          <span class="c-ck"><input type="checkbox" checked={allShownPicked} onchange={checkAll} title="Select all shown" /></span>
          <span></span>
          {@render sortHead('name', 'Name', 'c-name', '')}
          <span class="c-act"></span>
          {@render sortHead('author', 'Author', 'c-author', '')}
          {#if hasSide}{@render sortHead('side', 'Side', 'c-side', SIDE_HEAD)}{/if}
          {@render sortHead('on', 'On', 'c-on', "Off keeps the file but the server doesn't load it (renamed to .disabled). Click to sort.")}
          {@render sortHead('added', 'Added', 'c-added', '')}
          <span class="c-end"></span>
        </div>
        <div class="rows" data-pick-list="mods" bind:this={rowsEl} style:height="{shown.length * ROW}px" role="grid" tabindex="0"
          onkeydown={tableKey} onfocus={() => (focused = true)} onblur={() => (focused = false)}
          onpointermove={() => keys && (keys = false)}>
        <div class="win" style:transform="translateY({win.a * ROW}px)">
        {#each visible as m, vi (m.file)}
          {@const ix = win.a + vi}
          {@const up = pending(m) ? m.update : null}
          {@const vl = versionLocked(m)}
          {@const breaking = !!problems(up).length && !vl}
          {@const brk = up?.breaks || []}
          {@const swOn = swSweep === 'switch' && swept('mods', ix) ? pick.on : m.enabled}
          {@const sel = selected.has(m.file)}
          {@const ver = up ? null : m.short_version || m.version}
          {@const cats = m.categories || []}
          <div class="tr mod row-hover" class:off={!m.enabled} class:row-sel={sel} class:sel class:peeked={picked?.file === m.file && !sel} class:menu={menu?.m === m}
            class:kc={focused && cursor === ix} class:keys={keys && cursor === ix} class:yours={yours(m)} role="row" tabindex="-1" data-pick-ix={ix}
            onclick={(e) => rowClick(m, ix, e)}
            onkeydown={() => {}}
            oncontextmenu={(e) => openMenu(m, e)}>
            <span class="c-ck"><input type="checkbox" checked={sel} onpointerdown={(e) => tickDown(e, m, ix)} onclick={(e) => check(m, ix, e)} /></span>
            <button class="nmlink thumb" title={nameTip(m)} onclick={(e) => peek(m, e)}>{#if m.icon}<img src={m.icon} alt="" width="30" height="30" loading="lazy" />{:else}<span class="ph sm"></span>{/if}</button>
            <div class="c-name">
              <div class="nm">
                <button class="nmlink ell" title={nameTip(m)} onclick={(e) => peek(m, e)}>{m.name}</button>
                {#if hasSide && SIDE[m.side]}{@const I = SIDE[m.side][0]}<span class="side-in muted" title={SIDE[m.side][1]}><I size={12} /></span>{/if}
                {#if m.locked}<span class="lk" title="Updates ignored. Right-click to allow them."><Lock size={11} /></span>{/if}
              </div>
              <div class="tags">
                {#if isNew(m) && !yours(m)}<span class="chip tag okc">New</span>{/if}
                {#if yours(m)}<span class="chip tag gold">Yours</span>{/if}
                {#if !m.source}<span class="chip tag">Unknown</span>{/if}
                {#if SIDE_TAG[m.side]}<span class="chip tag {SIDE_TAG[m.side][1]}">{SIDE_TAG[m.side][0]}</span>{/if}
                {#if !(up && (vl || breaking))}
                  {#each cats.slice(0, 2) as c (c)}<span class="chip tag">{c}</span>{/each}
                  {#if cats.length > 2}<span class="tiny muted" title={cats.slice(2).join(', ')}>+{cats.length - 2}</span>{/if}
                {/if}
                {#if m.source === 'modrinth'}<span class="chip tag mr">Modrinth</span>{/if}
                {#if up}
                  <button class="vchip upc" class:warnv={breaking} class:lockv={vl} title={`${m.file}
Change version`} onclick={(e) => { e.stopPropagation(); versionsFor = m; }}><span class="ell muted">{up.from}</span><ArrowRight size={10} /><b class="ell">{up.to}</b><span class="chev"><ChevronDown size={10} /></span></button>
                  {#if vl}<button class="chip tag lockt" title="{up.to} is locked: you keep this version. Click to unlock." onclick={(e) => { e.stopPropagation(); lockVersion(m, false); }}><Lock size={10} />{up.to} locked</button>
                  {:else if breaking}
                    {#if brk.length}{@const bm = byName(brk[0].name)}<button class="chip tag brk" title={brk.map((b) => `${b.name} needs ${b.needs}`).join('; ')} onclick={(e) => { e.stopPropagation(); if (bm) peek(bm); }}>breaks {#if bm?.icon}<img src={bm.icon} alt="" width="12" height="12" />{/if}<span class="ell">{brk[0].name}</span>{#if brk.length > 1}<span>+{brk.length - 1}</span>{/if}</button>{/if}
                    <button class="chip tag lockt" title="Keep this version: the update stays listed, but only installs with Shift+click" onclick={(e) => { e.stopPropagation(); lockVersion(m, true); }}><Lock size={10} />Lock {up.to}</button>{/if}
                {:else if ver}
                  {#if m.source}<button class="vchip" title={`${m.file}
Change version`} onclick={(e) => { e.stopPropagation(); versionsFor = m; }}><span class="ell">{ver}</span><span class="chev"><ChevronDown size={10} /></span></button>
                  {:else}<span class="vchip plain" title={m.file}><span class="ell">{ver}</span></span>{/if}
                {/if}
              </div>
            </div>
            <div class="c-act">
              {#if done.has(m.project_id)}<span class="okt small row"><CircleCheck size={13} />Updated</span>
              {:else if up}
                <div class="splitb" class:warnb={breaking} class:lockb={vl}>
                  <button class="lead" class:lit={swSweep === 'action' && swept('mods', ix)} disabled={updating.has(m.project_id) || !!busy}
                    title={vl ? `${up.to} is locked: you keep ${up.from}.\nShift+click to update anyway, or click the lock tag to unlock.` : breaking ? `${up.from}  →  ${up.file}\nThis stops the server loading: ${problems(up).join('; ')}` : !m.enabled ? `${up.from}  →  ${up.file}\nIt stays disabled after updating: turn it on in the On column.` : `${up.from}  →  ${up.file}`}
                    onpointerdown={(e) => sweepable(m) && updateDown(e, m, ix)} onclick={(e) => updateOne(m, e)}>{#if updating.has(m.project_id)}<span class="spinner"></span>Updating{:else}{#if vl}<Lock size={12} />{:else if breaking}<TriangleAlert size={12} />{:else}<Download size={12} />{/if}Update{/if}</button>
                  <button onclick={() => (versionsFor = m)}>What changed</button>
                </div>
              {:else if m.source}
                <button class="btn sm ghostcl" onclick={() => (versionsFor = m)} title="Versions and changelogs"><ScrollText size={12} />Changelog</button>
              {/if}
            </div>
            <span class="c-author muted ell" title={m.author}>{m.author}</span>
            {#if hasSide}<span class="c-side muted" title={SIDE[m.side]?.[1] || 'Not known'}>{#if SIDE[m.side]}{@const I = SIDE[m.side][0]}<I size={13} />{/if}</span>{/if}
            <span class="c-on"><label class="switch" class:pend={swOn !== m.enabled} title={m.enabled ? 'Turn off' : 'Turn on'} onpointerdown={(e) => switchDown(e, m, ix)}><input type="checkbox" checked={swOn} onclick={(e) => toggle(m, e)} /><span></span></label></span>
            <span class="c-added muted" title={dateOf(m.date_added)}>{ago(m.date_added)}</span>
            <span class="c-end row">
              <button class="ib more" title="More" onclick={(e) => openMenu(m, e)}><Ellipsis size={14} /></button>
              <button class="ib del" title="Remove (kept in .hopper-backups)" onclick={async (e) => { e.stopPropagation(); if (await drop([m])) changed(true); }}><Trash2 size={13} /></button>
            </span>
          </div>
        {/each}
        </div>
        </div>
        {#if !data}
          <div class="skels">{#each [0, 1, 2, 3, 4, 5] as i (i)}<div class="skel"><i></i></div>{/each}</div>
        {:else if !shown.length}
          {@const filtered = !!filter.trim() || chip !== 'all' || (onlyYours && hasPack)}
          {#if chip === 'updates' && !filter.trim() && !(onlyYours && hasPack)}
            <div class="empty-state"><span class="es-ic"><CircleCheck size={20} /></span><b>All up to date</b><p>Every item here is on its newest version.</p><button class="btn sm" onclick={() => (chip = 'all')}>Show all</button></div>
          {:else if filtered}
            <div class="empty-state"><span class="es-ic"><Search size={20} /></span><b>Nothing matches</b><p>No item here fits the search and filters.</p><button class="btn sm" onclick={clearFilters}>Clear search and filters</button></div>
          {:else}
            <div class="empty-state"><span class="es-ic"><Package size={20} /></span><b>Nothing installed here yet</b><p>Add content finds mods and data packs for this server.</p><a class="btn sm" href="#{browseHash('mods', server.id)}"><Plus size={13} />Add content</a></div>
          {/if}
        {/if}
      </div>
      {#if pickedMod}<ModPeek {server} mod={pickedMod} onClose={() => (picked = null)} onChanged={changed} onUpdate={(m) => applyUpdates([m.project_id])} />{/if}
    </div>

    {#if selMods.length}
      <!-- Floats over the table, bottom centre, so the list doesn't jump. -->
      <div class="bulkw">
        <div class="bulk row">
          <b>{selMods.length} selected</b>
          <span class="vr"></span>
          <button class="btn sm ghost" onclick={() => bulk('on')}><CircleCheck size={13} />Enable</button>
          <button class="btn sm ghost" onclick={() => bulk('off')}><CircleOff size={13} />Disable</button>
          {#if selMods.some((m) => m.source)}<button class="btn sm ghost" title={allLocked ? 'Allow updates again' : 'Keep this version: updates are ignored'} onclick={() => bulk('lock')}>{#if allLocked}<LockOpen size={13} />Unlock{:else}<Lock size={13} />Lock{/if}</button>{/if}
          {#if selUpdates.length}<button class="btn sm pri" disabled={!!busy} onclick={() => bulk('update')}><Download size={13} />Update {selUpdates.length}</button>{/if}
          <button class="btn sm ghost bad" onclick={() => bulk('remove')}><Trash2 size={13} />Remove</button>
          <span class="vr"></span>
          <button class="btn sm ghost icon" title="Clear selection" onclick={() => (selected = new Set())}><X size={13} /></button>
        </div>
      </div>
    {/if}
  {/if}
</div>

{#if menu}
  {@const m = menu.m}
  {@const mup = pending(m) ? m.update : null}
  {@const mvl = versionLocked(m)}
  <div class="ctx popover menu" style:left="{menu.x}px" style:top="{menu.y}px" role="menu" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
    {#if m.source && browseOf(m)}<a class="menu-item" href="#{browseHash('mods', server.id)}/{m.source}/{encodeURIComponent(browseOf(m))}" onclick={() => (menu = null)}><Search size={13} />Open in Browse</a>{/if}
    {#if m.source}<button class="menu-item" onclick={() => act((m) => (versionsFor = m))}><Clock size={13} />Versions and changelogs</button>{/if}
    <button class="menu-item" onclick={() => act(() => (location.hash = `/server/${encodeURIComponent(server.id)}/files/mods`))}><FolderOpen size={13} />Show in Files</button>
    {#if m.source}<button class="menu-item" onclick={() => act(editConfig)}><Pencil size={13} />Edit config…</button>{/if}
    {#if m.source}<button class="menu-item" onclick={() => act((m) => resetConfig(base, m))}><Settings2 size={13} />Reset config…</button>{/if}
    {#if m.page}<a class="menu-item" href={m.page} target="_blank" rel="noreferrer" onclick={() => (menu = null)}><ExternalLink size={13} />Open on {site(m)}</a>{/if}
    {#if m.source}<button class="menu-item" onclick={() => act(async (m) => (await switchSource(base, m, alive, (s) => (busy = s))) && changed(true))}><Shuffle size={13} />Switch to {m.source === 'modrinth' ? 'CurseForge' : 'Modrinth'}</button>{/if}
    {#if mup}<button class="menu-item" onclick={() => act((m) => lockVersion(m, !mvl))}>{#if mvl}<LockOpen size={13} /><span class="ell">Unlock {mup.to}</span>{:else}<Lock size={13} /><span class="ell">Lock {mup.to} (keep {mup.from})</span>{/if}</button>{/if}
    {#if m.source}<button class="menu-item" onclick={() => act(async (m) => (await setLocked(base, [m], !m.locked)) && changed(true))}>{#if m.locked}<LockOpen size={13} />Allow updates{:else}<Lock size={13} />Ignore updates{/if}</button>{/if}
    <button class="menu-item" onclick={() => act((m) => toggle(m))}><span class="noi"></span>{m.enabled ? 'Disable' : 'Enable'}</button>
    <div class="menu-sep"></div>
    <button class="menu-item bad" onclick={() => act(async (m) => (await drop([m])) && changed(true))}><Trash2 size={13} />Remove</button>
  </div>
{/if}

{#if loaderOpen}<LoaderVersions {server} onClose={() => (loaderOpen = false)} onChanged={() => changed(true)} />{/if}
{#if versionsFor}<ModVersions {server} mod={versionsFor} dialog onClose={() => (versionsFor = null)} onChanged={changed} />{/if}
{#if cfgPick}<ModsConfigPick mod={cfgPick.mod} files={cfgPick.files} onPick={openEditor} onClose={() => (cfgPick = null)} />{/if}
{#if reviewing?.length}<ReviewUpdates {server} updates={reviewing} {mods} {busy} onApply={applyUpdates} onClose={() => (reviewing = null)} />{/if}

<style>
  .card { display: flex; flex-direction: column; gap: 8px; }
  .subtabs { display: flex; align-items: center; gap: 4px; border-bottom: 1px solid var(--line); }
  /* Tabs: underline (app.css .tab-sel, desktop's tab_selected). */
  .tab { padding: 6px 12px; color: var(--subtle); }
  .tab:hover { color: var(--text); }
  .subtabs .chip { margin-bottom: 6px; }
  .toolbar { gap: 6px 10px; }
  .cbl { display: inline-flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--subtle); cursor: pointer; padding: 0 4px; }
  .cbl input, .c-ck input { width: 15px; height: 15px; accent-color: var(--acc); }
  .upd { gap: 4px; flex: none; }
  .meta { gap: 4px; }
  .find { padding: 3px 8px; width: 224px; font-weight: 400; }
  .clr { color: var(--muted); display: grid; }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .warn { color: var(--gold); }
  .okt { color: var(--green); gap: 5px; }
  .link { color: var(--acc); text-decoration: underline; font-size: 12px; }
  .banner { display: flex; align-items: center; gap: 8px; border: 1px solid #60a5fa44; background: var(--blue-dim); border-radius: 8px; padding: 6px 10px; color: var(--subtle); }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spinner.blue { color: var(--blue); }
  .bulkw { position: sticky; bottom: 16px; display: flex; justify-content: center; z-index: 30; pointer-events: none; animation: rise 0.16s ease-out; }
  @keyframes rise { from { opacity: 0; transform: translateY(8px); } }
  .bulk { pointer-events: auto; gap: 4px; background: var(--raise); border: 1px solid var(--line2); border-radius: 10px; padding: 6px 8px; flex-wrap: wrap; box-shadow: 0 14px 40px #000c; max-width: 100%; }
  .bulk b { padding: 0 8px; font-size: 12.5px; }
  .vr { width: 1px; height: 20px; background: var(--line2); margin: 0 2px; }
  .bad { color: var(--red); }
  .split { display: flex; gap: 12px; align-items: flex-start; }
  .list { flex: 1; min-width: 0; container-type: inline-size; }
  /* Desktop's columns: check, icon, name, Update / Changelog, Author, Side, On, Added, menu. */
  .tr { display: grid; grid-template-columns: 22px 30px minmax(0, 1fr) 190px 130px 40px 44px 86px 52px; gap: 10px; align-items: center; padding: 6px 8px; position: relative; border-radius: 8px; }
  .noside .tr { grid-template-columns: 22px 30px minmax(0, 1fr) 190px 130px 44px 86px 52px; }
  .th { color: var(--muted); font-size: 11px; padding-top: 2px; padding-bottom: 6px; border-bottom: 1px solid var(--line); border-radius: 0; }
  .th button { display: inline-flex; align-items: center; gap: 3px; color: var(--muted); font-size: 11.5px; font-weight: 500; text-align: left; }
  .th button:hover, .th button.act { color: var(--text); }
  .mod { cursor: default; height: 52px; overflow: hidden; }
  .rows { position: relative; outline: none; }
  .rows:focus-visible { box-shadow: none; }
  .win { will-change: transform; }
  /* Hover and selected: app.css .row-hover / .row-sel. The side panel's row keeps the hover wash. */
  .mod.peeked { background-color: var(--hover); }
  .mod.kc { box-shadow: inset 0 0 0 2px var(--acc); }
  .mod.yours::before { content: ''; position: absolute; left: 0; top: 10px; bottom: 10px; width: 2px; border-radius: 2px; background: var(--gold); }
  .mod.sel.yours::before { display: none; }
  .mod.off .c-name, .mod.off .thumb { opacity: 0.55; }
  .thumb { display: grid; border-radius: 6px; }
  .mod img { border-radius: 6px; background: var(--raise); flex: none; display: block; }
  .nm { font-weight: 500; display: flex; gap: 6px; align-items: center; min-width: 0; }
  .nmlink { text-align: left; font-weight: 500; cursor: pointer; }
  .nm .nmlink:hover { color: var(--acc); }
  .side-in { display: none; }
  .lk { color: var(--muted); display: grid; }
  .tags { display: flex; gap: 4px; align-items: center; margin-top: 2px; min-width: 0; overflow: hidden; flex-wrap: nowrap; }
  .tag { font-size: 10.5px; padding: 0 6px; flex: none; }
  .tag.gold { color: var(--gold); background: var(--gold-dim); }
  .tag.warnc { color: var(--gold); background: var(--gold-dim); }
  .tag.info { color: var(--blue); background: var(--blue-dim); }
  .tag.okc, .tag.mr { color: var(--green); background: var(--green-dim); }
  .lockt { gap: 3px; cursor: pointer; }
  .brk { gap: 4px; color: var(--red); background: var(--red-dim); max-width: 160px; min-width: 0; flex: 0 1 auto; }
  .brk img { border-radius: 3px; flex: none; }
  .lockt:hover { color: var(--text); }
  /* The version chip: click to pick another version; with an update, installed → offered. */
  .vchip { display: inline-flex; align-items: center; gap: 4px; flex: 0 1 auto; max-width: 200px; min-width: 0; padding: 0 6px; border-radius: 6px; font-size: 10.5px; line-height: 17px; background: #ffffff0d; color: var(--muted); }
  .vchip .chev { visibility: hidden; display: grid; color: var(--acc); flex: none; }
  .vchip:not(.plain):hover { background: var(--acc-dim); color: var(--acc); }
  .vchip:hover .chev { visibility: visible; }
  .vchip.upc { background: var(--acc-dim); color: var(--acc); }
  .vchip.upc b { font-weight: 600; }
  .vchip.upc:hover { background: #f9731640; }
  .vchip.warnv { background: var(--gold-dim); color: var(--gold); }
  .vchip.lockv { background: #ffffff0d; color: var(--muted); }
  .c-act { display: flex; justify-content: flex-start; }
  .c-on { display: flex; }
  .c-end { gap: 2px; justify-content: flex-end; }
  .ib { color: var(--muted); padding: 3px; border-radius: 5px; display: grid; }
  .ib:hover { color: var(--text); background: #ffffff10; }
  .ib.del:hover { color: var(--red); }
  /* Hover-only controls stay shown on the picked row, the key row and the row whose menu is open. */
  .mod .more, .mod .ghostcl { visibility: hidden; }
  .mod:hover .more, .mod:hover .ghostcl, .mod.sel .more, .mod.sel .ghostcl, .mod.keys .more, .mod.keys .ghostcl, .mod.menu .more, .mod.menu .ghostcl, .mod.peeked .more, .mod.peeked .ghostcl { visibility: visible; }
  .ghostcl { color: var(--subtle); gap: 5px; }
  .splitb { display: inline-flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; font-size: 12px; max-width: 100%; }
  .splitb button { padding: 3px 8px; display: inline-flex; align-items: center; gap: 5px; white-space: nowrap; color: var(--subtle); }
  .splitb button:hover:not(:disabled) { background: #ffffff10; color: var(--text); }
  .splitb .lead { color: var(--text); border-right: 1px solid var(--line2); }
  .splitb button:disabled { opacity: 0.7; }
  .splitb .lead.lit { background: var(--acc); color: #1a1a1f; }
  .splitb.warnb { border-color: #eab30866; }
  .splitb.warnb .lead { color: var(--gold); border-right-color: #eab30866; }
  .splitb.lockb .lead { opacity: 0.6; }
  .switch.pend span { outline: 2px dashed var(--acc); outline-offset: 2px; }
  @container (max-width: 1050px) {
    .tr { grid-template-columns: 22px 30px minmax(0, 1fr) 190px 40px 44px 86px 52px; } .tr .c-author { display: none; }
    .noside .tr { grid-template-columns: 22px 30px minmax(0, 1fr) 190px 44px 86px 52px; }
  }
  @container (max-width: 860px) {
    .tr { grid-template-columns: 22px 30px minmax(0, 1fr) 190px 40px 44px 52px; } .tr .c-added { display: none; }
    .noside .tr { grid-template-columns: 22px 30px minmax(0, 1fr) 190px 44px 52px; }
  }
  /* Narrow: the Side icon moves next to the name, Update shrinks to its icon. */
  @container (max-width: 640px) {
    .tr, .noside .tr { grid-template-columns: 22px 30px minmax(0, 1fr) 110px 44px 52px; } .tr .c-side { display: none; } .side-in { display: grid; }
    .splitb button:not(.lead) { display: none; } .ghostcl { font-size: 0; gap: 0; }
  }
  .ph { width: 34px; height: 34px; border-radius: 6px; background: var(--raise); flex: none; display: block; }
  .ph.sm { width: 30px; height: 30px; }
  .icon { padding: 3px 5px; }
  /* Loading rows: app.css .skel (desktop's skeleton_rows). */
  .skels { display: flex; flex-direction: column; gap: 4px; padding-top: 6px; }
  .ctx { position: fixed; z-index: 80; width: 250px; display: flex; flex-direction: column; }
  .ctx .menu-item { min-width: 0; }
  .ctx .bad { color: var(--red); }
  .noi { width: 13px; flex: none; }
</style>
