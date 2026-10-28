<script>
  // Files, like a file explorer: back / forward / up (also mouse buttons 4/5
  // and Alt+←/→), a path you can type, a folder tree, list or tiles, a
  // preview pane, a right-click menu, cut/copy/paste, drag to move (Ctrl to
  // copy), rubber-band select, and double-click opens by type. Uploads are
  // resumable; deletes go to the server's trash.
  import { onMount, onDestroy, untrack } from 'svelte';
  import {
    Folder, FolderOpen, File, FileText, FileArchive, Image, HardDrive, Search, FolderPlus, Upload, FolderUp,
    Download, Copy, Pencil, Trash2, RefreshCw, ArrowUp, ArrowLeft, ArrowRight, FilePlus, TextSearch, X,
    List, LayoutGrid, PanelLeft, PanelRight, Scissors, ClipboardPaste, Info, Package, ExternalLink, Layers, TriangleAlert,
    Link,
  } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { say } from './lib/toast.svelte.js';
  import { ask, confirmIt, dialog } from './lib/dialog.svelte.js';
  import { queueUploads, onUploaded, download } from './lib/transfers.svelte.js';
  import { historyOf, clip, prefs, store, jump, TEXT, ARCHIVE, PACKED, IMAGE } from './lib/files.svelte.js';
  import { configOwners, ownerOf } from './lib/files-config.js';
  import { startPick, clickSwallowed, tickLabel } from './lib/dragpick.svelte.js';
  import FileTree from './FileTree.svelte';
  import FileViewer from './FileViewer.svelte';
  import ModPeek from './ModPeek.svelte';

  let { server, path = '', go, openFile, openTrash } = $props();
  // The live feed hands over a new `server` object every second; effects
  // that should run once per server key on the id alone.
  const sid = $derived(server.id);

  const ROW = 34;
  let entries = $state([]);
  let loading = $state(true);
  let error = $state('');
  let filter = $state('');
  let selected = $state(new Set());
  let last = $state(null);
  let dragging = $state(false);
  let scroller = $state();
  let scrollTop = $state(0);
  let viewH = $state(600);
  let filePick = $state();
  let folderPick = $state();
  let found = $state(null);
  let finding = $state(false);
  let regex = $state(false);
  let editingPath = $state(false);
  let pathText = $state('');
  let ctx = $state(null);
  let viewer = $state(null);
  let props = $state(null);
  let preview = $state(null);
  let overDir = $state(null);
  let band = $state(null);
  let treeVersion = $state(0);
  /** Which mod each entry of config/ belongs to (files-config.js). */
  let owners = $state(null);
  /** The mod whose panel is open beside the list (its jar's name). */
  let peekJar = $state('');

  const base = $derived(`/fs/${encodeURIComponent(server.id)}`);
  const crumbs = $derived(path ? path.split('/') : []);
  const shown = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    return q ? entries.filter((e) => e.name.toLowerCase().includes(q)) : entries;
  });
  // In config/, files sit under the mod they belong to, like desktop opens
  // a mod's config from the mod: a heading row per mod, then its files.
  const inConfig = $derived(path === 'config');
  const grouped = $derived(inConfig && prefs.group && prefs.view === 'list' && !!owners);
  const modOf = (e, dir = path) => ownerOf(owners, join(e.name, dir));
  /** The rows the list draws: entries, and mod headings (with a key) when grouped. */
  const items = $derived.by(() => {
    if (!grouped) return shown;
    const by = new Map();
    const rest = [];
    for (const e of shown) {
      const m = modOf(e);
      if (!m) rest.push(e);
      else {
        if (!by.has(m.file)) by.set(m.file, { m, list: [] });
        by.get(m.file).list.push(e);
      }
    }
    const out = [];
    for (const { m, list } of [...by.values()].sort((a, b) => a.m.name.localeCompare(b.m.name))) {
      out.push({ head: m, n: list.length, key: `@${m.file}` });
      out.push(...list);
    }
    if (rest.length && out.length) out.push({ head: null, n: rest.length, key: '@rest' });
    out.push(...rest);
    return out;
  });
  /** The entries in the order they're drawn (shift-click ranges, Ctrl+A). */
  const ordered = $derived(grouped ? items.filter((x) => x.key == null) : shown);
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - 10));
  const slice = $derived(items.slice(first, first + Math.ceil(viewH / ROW) + 20));
  const selList = $derived(entries.filter((e) => selected.has(e.name)));
  const selSize = $derived(selList.reduce((n, e) => n + e.size, 0));
  const hist = $derived(historyOf(sid));
  let histTick = $state(0);
  const canBack = $derived((histTick, hist.back.length > 0));
  const canFwd = $derived((histTick, hist.fwd.length > 0));

  const join = (name, dir = path) => (dir ? `${dir}/${name}` : name);
  const parentOf = (p) => p.split('/').slice(0, -1).join('/');

  // ---- navigation --------------------------------------------------------------

  let prevPath = null;
  let viaHistory = false;
  function navTo(p) {
    if (p !== path) go(p);
  }
  function back() {
    if (!hist.back.length) return;
    hist.fwd.push(path);
    viaHistory = true;
    histTick++;
    go(hist.back.pop());
  }
  function forward() {
    if (!hist.fwd.length) return;
    hist.back.push(path);
    viaHistory = true;
    histTick++;
    go(hist.fwd.pop());
  }
  function up() {
    if (path) navTo(parentOf(path));
  }

  /** A name to select once the folder has loaded (a search hit on a file's name). */
  let reveal = '';
  let loadSeq = 0;
  async function load() {
    const seq = ++loadSeq;
    loading = true;
    error = '';
    try {
      const r = await api(`${base}/list?path=${encodeURIComponent(path)}`);
      // A slower answer for a folder already left behind.
      if (seq !== loadSeq) return;
      entries = r.entries;
      if (reveal && entries.some((e) => e.name === reveal)) {
        selected = new Set([reveal]);
        last = reveal;
        const ix = items.findIndex((e) => e.key == null && e.name === reveal);
        if (scroller && prefs.view === 'list' && ix >= 0) scroller.scrollTop = Math.max(0, ix * ROW - viewH / 3);
      } else {
        selected = new Set([...selected].filter((n) => entries.some((e) => e.name === n)));
      }
      reveal = '';
    } catch (e) {
      if (seq !== loadSeq) return;
      error = e.message;
      entries = [];
    } finally {
      if (seq === loadSeq) loading = false;
    }
  }

  function refresh() {
    load();
    treeVersion++;
  }

  // Runs on a new folder or server only: everything else it touches is
  // untracked (the list's element comes and goes with search results, and
  // the history counter is written here), so it never loads twice.
  let prevSid = null;
  $effect(() => {
    const p = path;
    const s = sid;
    untrack(() => {
      if (s !== prevSid) {
        prevSid = s;
        prevPath = null;
        owners = null;
        peekJar = '';
      }
      if (prevPath !== null && prevPath !== p && !viaHistory) {
        hist.back.push(prevPath);
        if (hist.back.length > 100) hist.back.shift();
        hist.fwd.length = 0;
        histTick++;
      }
      viaHistory = false;
      prevPath = p;
      selected = new Set();
      filter = '';
      found = null;
      preview = null;
      editingPath = false;
      if (scroller) scroller.scrollTop = 0;
      load();
    });
  });

  // Mods' icons and names beside config files, loaded the first time
  // config/ (or a search) shows any.
  $effect(() => {
    const want = path === 'config' || path.startsWith('config/') || (found?.hits || []).some((h) => h.path.startsWith('config/'));
    const s = sid;
    if (want && !owners) configOwners(s).then((o) => s === sid && (owners = o));
  });
  const peekMod = $derived(peekJar ? owners?.mods.get(peekJar) : null);
  /** A mod's panel beside the list (the Mods tab when it isn't listed there). */
  function showMod(m) {
    if (!owners?.mods.get(m.file)) return openMods();
    peekJar = peekJar === m.file ? '' : m.file;
  }
  /** Opens a search hit at its line. */
  function openHit(p, line = 0) {
    // A name hit on something that isn't text (a jar, an image, a folder's
    // file): show it in its folder, selected, instead of opening it as text.
    if (!line && !TEXT.test(p)) {
      const dir = parentOf(p);
      reveal = p.split('/').pop();
      found = null;
      if (dir !== path) navTo(dir);
      else load();
      return;
    }
    jump.path = p;
    jump.line = line;
    openFile(p, 0);
  }

  function startEditPath() {
    pathText = path;
    editingPath = true;
  }
  function submitPath(e) {
    e.preventDefault();
    editingPath = false;
    navTo(pathText.trim().replace(/\\/g, '/').replace(/^\/+|\/+$/g, ''));
  }

  // ---- what things are -----------------------------------------------------------

  function icon(e) {
    if (e.dir) return Folder;
    if (TEXT.test(e.name)) return FileText;
    if (ARCHIVE.test(e.name) || PACKED.test(e.name)) return FileArchive;
    if (IMAGE.test(e.name)) return Image;
    return File;
  }
  function kind(e) {
    if (e.dir) return 'Folder';
    if (/\.jar$/i.test(e.name)) return path.startsWith('mods') ? 'Mod' : 'Jar';
    if (ARCHIVE.test(e.name)) return 'Zip';
    if (IMAGE.test(e.name)) return 'Image';
    if (/\.log$/i.test(e.name)) return 'Log';
    if (TEXT.test(e.name)) return (e.name.split('.').pop() || 'Text').toUpperCase();
    return (e.name.includes('.') ? e.name.split('.').pop().toUpperCase() : 'File');
  }

  function when(t) {
    if (!t) return '';
    const d = new Date(t * 1000);
    const age = (Date.now() - d) / 86400000;
    if (age < 1 && d.getDate() === new Date().getDate()) return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    if (age < 7) return `${Math.max(1, Math.round(age))} d ago`;
    return d.toLocaleDateString();
  }
  const fileUrl = (p, inline = false) => `/api/v1${base}/file?path=${encodeURIComponent(p)}${inline ? '&inline=1' : ''}`;

  // ---- selecting ------------------------------------------------------------------

  let suppressClick = false;
  function click(e, ev) {
    // A tick sweep let go over a row's own name lands its click on the row.
    if (suppressClick || clickSwallowed()) return;
    const s = new Set(selected);
    if (ev.shiftKey && last) {
      const a = ordered.findIndex((x) => x.name === last);
      const b = ordered.findIndex((x) => x.name === e.name);
      for (const x of ordered.slice(Math.min(a, b), Math.max(a, b) + 1)) s.add(x.name);
    } else if (ev.ctrlKey || ev.metaKey) {
      s.has(e.name) ? s.delete(e.name) : s.add(e.name);
    } else {
      s.clear();
      s.add(e.name);
    }
    last = e.name;
    selected = s;
  }

  // Press a row's tick box and drag across rows: each row crossed takes the
  // same tick (desktop's drag_pick).
  function tickDown(ev, e, ix) {
    const on = !selected.has(e.name);
    startPick(ev, {
      list: 'files', kind: 'tick', ix, on, selected,
      keyAt: (i) => (items[i] && items[i].key == null ? items[i].name : null),
      onTick: (s) => (selected = s),
      label: tickLabel(on),
    });
  }

  function toggle(e) {
    const s = new Set(selected);
    s.has(e.name) ? s.delete(e.name) : s.add(e.name);
    last = e.name;
    selected = s;
  }

  // Rubber band: drag on empty space (or a row's free space) to select.
  function bandStart(ev) {
    if (ev.button !== 0 || ev.target.closest('.dragh, input, button, a')) return;
    const box = scroller.getBoundingClientRect();
    band = { x0: ev.clientX - box.left + scroller.scrollLeft, y0: ev.clientY - box.top + scroller.scrollTop, x1: 0, y1: 0, live: false, add: ev.ctrlKey || ev.shiftKey, base: new Set(selected) };
    band.x1 = band.x0;
    band.y1 = band.y0;
  }
  function bandMove(ev) {
    if (!band) return;
    const box = scroller.getBoundingClientRect();
    band.x1 = ev.clientX - box.left + scroller.scrollLeft;
    band.y1 = ev.clientY - box.top + scroller.scrollTop;
    if (!band.live && Math.abs(band.x1 - band.x0) + Math.abs(band.y1 - band.y0) > 6) band.live = true;
    if (!band.live) return;
    const top = Math.min(band.y0, band.y1);
    const bottom = Math.max(band.y0, band.y1);
    const s = band.add ? new Set(band.base) : new Set();
    if (prefs.view === 'list') {
      const head = 0;
      const a = Math.max(0, Math.floor((top - head) / ROW));
      const b = Math.min(items.length - 1, Math.floor((bottom - head) / ROW));
      for (let i = a; i <= b; i++) if (items[i].key == null) s.add(items[i].name);
    } else {
      const left = Math.min(band.x0, band.x1) + box.left - scroller.scrollLeft;
      const right = Math.max(band.x0, band.x1) + box.left - scroller.scrollLeft;
      const t = top + box.top - scroller.scrollTop;
      const bt = bottom + box.top - scroller.scrollTop;
      for (const el of scroller.querySelectorAll('[data-name]')) {
        const r = el.getBoundingClientRect();
        if (r.right > left && r.left < right && r.bottom > t && r.top < bt) s.add(el.dataset.name);
      }
    }
    selected = s;
  }
  function bandEnd() {
    if (band?.live) {
      suppressClick = true;
      setTimeout(() => (suppressClick = false), 0);
    } else if (band && !band.add) {
      // A plain click on empty space clears the selection.
      if (!suppressClick) selected = new Set();
    }
    band = null;
  }

  // ---- opening ---------------------------------------------------------------------

  function open(e) {
    const p = join(e.name);
    if (e.dir) navTo(p);
    else if (IMAGE.test(e.name)) viewer = { mode: 'image', path: p, images: shown.filter((x) => !x.dir && IMAGE.test(x.name)).map((x) => join(x.name)) };
    else if (ARCHIVE.test(e.name)) viewer = { mode: 'zip', path: p };
    else if (TEXT.test(e.name) || e.size < 64 * 1024) openFile(p, e.size);
    else download(fileUrl(p));
  }

  // The preview pane follows a single selection.
  let previewFor = '';
  $effect(() => {
    if (!prefs.preview || selList.length !== 1) {
      preview = null;
      previewFor = '';
      return;
    }
    const e = selList[0];
    const p = join(e.name);
    if (p === previewFor) return;
    previewFor = p;
    const it = { e, p };
    preview = it;
    if (e.dir) return;
    if (TEXT.test(e.name) && e.size < 512 * 1024) {
      api(`${base}/text?path=${encodeURIComponent(p)}`).then((r) => previewFor === p && (preview = { ...it, text: r.content.split('\n').slice(0, 80).join('\n') }), () => {});
    } else if (ARCHIVE.test(e.name)) {
      api(`${base}/zip-list?path=${encodeURIComponent(p)}`).then((r) => previewFor === p && (preview = { ...it, zip: r }), () => {});
    }
  });

  // ---- operations ------------------------------------------------------------------

  async function ops(list, message) {
    try {
      await post(`${base}/ops`, { ops: list });
      if (message) say(message);
    } catch (e) {
      say(e.message, true);
    }
    refresh();
  }

  async function mkdir() {
    const name = await ask('New folder', '', { ok: 'Create', placeholder: 'Folder name' });
    if (name?.trim()) ops([{ op: 'mkdir', path: join(name.trim()) }]);
  }

  async function newFile() {
    const name = await ask('New file', '', { ok: 'Create', placeholder: 'name.toml' });
    const rel = join(name?.trim().replace(/\\/g, '/').replace(/^\/+|\/+$/g, '') || '');
    if (!name?.trim() || rel === path) return;
    try {
      // A name that's taken opens that file; it's never emptied.
      const dir = parentOf(rel);
      const leaf = rel.split('/').pop();
      const here = dir === path ? entries : await api(`${base}/list?path=${encodeURIComponent(dir)}`).then((r) => r.entries, () => []);
      const there = here.find((e) => e.name.toLowerCase() === leaf.toLowerCase());
      if (there?.dir) return say(`${leaf} is a folder.`);
      if (there) {
        say(`${rel.split('/').pop()} already exists, so it opened as it is.`);
      } else {
        // Empty text isn't valid JSON or TOML yet, hence force.
        await api(`${base}/text`, { method: 'PUT', body: { path: rel, content: '', force: true } });
      }
      openFile(rel, 0);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function rename() {
    const e = selList[0];
    const name = await ask(`Rename ${e.name}`, e.name, { ok: 'Rename' });
    if (name?.trim() && name.trim() !== e.name) ops([{ op: 'rename', path: join(e.name), name: name.trim() }]);
  }

  async function moveOrCopy(op) {
    const to = await ask(`${op === 'move' ? 'Move' : 'Copy'} ${selList.length === 1 ? selList[0].name : `${selList.length} items`} to…`, path, {
      body: 'A folder in this server, like mods/disabled. Leave empty for the server folder.',
      ok: op === 'move' ? 'Move' : 'Copy',
    });
    if (to === null) return;
    ops([{ op, paths: selList.map((e) => join(e.name)), to: to.trim().replace(/^\/+|\/+$/g, '') }], `${op === 'move' ? 'Moved' : 'Copied'} ${selList.length}`);
  }

  async function trash() {
    const n = selList.length;
    const ok = await confirmIt(`Move ${n === 1 ? selList[0].name : `${n} items`} to the trash?`, {
      body: 'You can restore it from the trash for 30 days.',
      ok: 'Move to trash',
      danger: true,
    });
    if (ok) ops([{ op: 'trash', paths: selList.map((e) => join(e.name)) }], `Moved ${n} to the trash`);
  }

  function downloadSel(list = selList) {
    if (list.length === 1 && !list[0].dir) {
      download(fileUrl(join(list[0].name)));
    } else {
      const q = list.map((e) => `path=${encodeURIComponent(join(e.name))}`).join('&');
      download(`/api/v1${base}/zip?${q}`);
    }
  }

  function cutCopy(op) {
    if (!selList.length) return;
    clip.op = op;
    clip.sid = sid;
    clip.paths = selList.map((e) => join(e.name));
    clip.from = path;
    say(`${op === 'move' ? 'Cut' : 'Copied'} ${selList.length}: paste into another folder with Ctrl+V.`);
  }

  function paste(to = path) {
    if (!clip.paths.length || clip.sid !== sid) return;
    if (clip.op === 'move' && clip.from === to) return say('They are in this folder already.');
    const n = clip.paths.length;
    ops([{ op: clip.op, paths: clip.paths, to }], `${clip.op === 'move' ? 'Moved' : 'Copied'} ${n}`);
    if (clip.op === 'move') clip.paths = [];
  }

  async function copyPath(p) {
    try {
      await navigator.clipboard.writeText(p);
      say(`Copied ${p}`);
    } catch {
      say(p);
    }
  }

  // ---- uploads and drag and drop ---------------------------------------------------

  /** Uploads into `dir`, asking first if top-level names already exist there. */
  async function upload(items, dir = path) {
    if (!items.length) return;
    let names = new Set(entries.map((e) => e.name));
    if (dir !== path) {
      try {
        names = new Set((await api(`${base}/list?path=${encodeURIComponent(dir)}`)).entries.map((e) => e.name));
      } catch {
        names = new Set();
      }
    }
    const clash = [...new Set(items.map((i) => i.rel.split('/')[0]))].filter((n) => names.has(n));
    let overwrite = false;
    if (clash.length) {
      overwrite = await confirmIt(`Replace ${clash.length === 1 ? clash[0] : `${clash.length} items`}?`, {
        body: `${clash.slice(0, 5).join(', ')}${clash.length > 5 ? '…' : ''} already exist there. The old copies go to the trash.`,
        ok: 'Replace',
      });
      if (!overwrite) items = items.filter((i) => !clash.includes(i.rel.split('/')[0]));
    }
    queueUploads(server.id, items.map((i) => ({ file: i.file, path: join(i.rel, dir) })), overwrite);
  }

  function picked(ev, folders) {
    const items = [...ev.target.files].map((f) => ({ file: f, rel: folders ? f.webkitRelativePath : f.name }));
    ev.target.value = '';
    upload(items);
  }

  async function readEntry(entry, prefix, out) {
    if (entry.isFile) {
      const file = await new Promise((res, rej) => entry.file(res, rej));
      out.push({ file, rel: prefix + entry.name });
    } else if (entry.isDirectory) {
      const reader = entry.createReader();
      let batch;
      do {
        batch = await new Promise((res, rej) => reader.readEntries(res, rej));
        for (const e of batch) await readEntry(e, `${prefix}${entry.name}/`, out);
      } while (batch.length);
    }
  }

  const INTERNAL = 'application/x-hopperd-files';
  function dragStart(ev, e) {
    if (!selected.has(e.name)) {
      selected = new Set([e.name]);
      last = e.name;
    }
    const paths = [...selected].map((n) => join(n));
    ev.dataTransfer.setData(INTERNAL, JSON.stringify({ sid, paths }));
    ev.dataTransfer.effectAllowed = 'copyMove';
    // Chrome and Edge can drop a single file onto the desktop as a download.
    if (paths.length === 1 && !e.dir) {
      ev.dataTransfer.setData('DownloadURL', `application/octet-stream:${e.name}:${location.origin}${fileUrl(paths[0])}`);
    }
  }

  /** A drop on a folder (row, tile, tree or crumb): moves/copies our files, uploads files from outside. */
  async function dropOn(ev, dir) {
    ev.preventDefault();
    ev.stopPropagation();
    dragging = false;
    overDir = null;
    const raw = ev.dataTransfer.getData(INTERNAL);
    if (raw) {
      const { sid: from, paths } = JSON.parse(raw);
      if (from !== sid) return say('Drag between folders of the same server.');
      const moving = paths.filter((p) => p !== dir && !dir.startsWith(`${p}/`) && parentOf(p) !== dir);
      if (!moving.length) return;
      const op = ev.ctrlKey ? 'copy' : 'move';
      ops([{ op, paths: moving, to: dir }], `${op === 'move' ? 'Moved' : 'Copied'} ${moving.length} to ${dir || server.name}`);
      return;
    }
    const roots = [...ev.dataTransfer.items].map((i) => i.webkitGetAsEntry?.()).filter(Boolean);
    const out = [];
    for (const r of roots) await readEntry(r, '', out);
    upload(out, dir);
  }

  function dirOver(ev, e) {
    if (!e.dir) return;
    ev.preventDefault();
    ev.stopPropagation();
    ev.dataTransfer.dropEffect = ev.dataTransfer.types.includes(INTERNAL) ? (ev.ctrlKey ? 'copy' : 'move') : 'copy';
    overDir = e.name;
  }

  // ---- right-click menu ------------------------------------------------------------

  function menu(ev, e = null) {
    ev.preventDefault();
    if (e && !selected.has(e.name)) {
      selected = new Set([e.name]);
      last = e.name;
    }
    if (!e) selected = new Set();
    const w = 220;
    const h = e ? 330 : 220;
    ctx = { x: Math.min(ev.clientX, innerWidth - w - 8), y: Math.min(ev.clientY, innerHeight - h - 8), e };
  }
  function act(fn) {
    ctx = null;
    fn();
  }

  function showProps(list) {
    const one = list.length === 1 ? list[0] : null;
    props = one
      ? { title: one.name, rows: [['Kind', kind(one)], ['Where', join(one.name)], ...(one.dir ? [] : [['Size', `${bytes(one.size)} (${one.size.toLocaleString()} bytes)`]]), ['Changed', one.modified ? new Date(one.modified * 1000).toLocaleString() : '']] }
      : { title: `${list.length} items`, rows: [['Where', path || '/'], ['Files', String(list.filter((x) => !x.dir).length)], ['Folders', String(list.filter((x) => x.dir).length)], ['Size of the files', bytes(list.reduce((n, x) => n + x.size, 0))]] };
  }

  // ---- keys and mouse buttons ------------------------------------------------------

  function keys(ev) {
    // A dialog (rename, confirm) has the keys: Enter there mustn't also open
    // the selected file, nor Delete trash it again.
    if (props && ev.key === 'Escape' && !dialog.open) {
      props = null;
      return;
    }
    if (viewer || props || dialog.open || ev.defaultPrevented) return;
    if (['INPUT', 'TEXTAREA', 'SELECT'].includes(ev.target.tagName) || ev.target.isContentEditable) return;
    if (ev.key === 'Enter' && ev.target.closest?.('button, a, [role="button"]')) return;
    const mod = ev.ctrlKey || ev.metaKey;
    // Ctrl+C / X on text picked in the preview copies that text.
    if (mod && (ev.key === 'c' || ev.key === 'x') && String(getSelection?.() || '').trim()) return;
    if (ev.altKey && ev.key === 'ArrowLeft') { ev.preventDefault(); back(); }
    else if (ev.altKey && ev.key === 'ArrowRight') { ev.preventDefault(); forward(); }
    else if (ev.altKey && ev.key === 'ArrowUp') { ev.preventDefault(); up(); }
    else if (ev.altKey && ev.key === 'Enter' && selList.length) { ev.preventDefault(); showProps(selList); }
    else if (ev.key === 'Escape') {
      if (ctx) ctx = null;
      else if (peekJar) peekJar = '';
      else selected = new Set();
    }
    else if (ev.key === 'Delete' && selList.length) trash();
    else if (ev.key === 'F2' && selList.length === 1) rename();
    else if (ev.key === 'F5') { ev.preventDefault(); refresh(); }
    else if (ev.key === 'Backspace' && path) up();
    else if (mod && ev.key === 'a') { ev.preventDefault(); selected = new Set(ordered.map((e) => e.name)); }
    else if (mod && ev.key === 'x') cutCopy('move');
    else if (mod && ev.key === 'c') cutCopy('copy');
    else if (mod && ev.key === 'v') paste();
    else if (ev.key === 'Enter' && selList.length === 1) open(selList[0]);
  }
  function mouseNav(ev) {
    if (ev.button === 3 || ev.button === 4) {
      ev.preventDefault();
      if (ev.type === 'mouseup') ev.button === 3 ? back() : forward();
    }
  }

  let off;
  onMount(() => {
    off = onUploaded((s, folder) => s === server.id && folder === path && refresh());
    addEventListener('keydown', keys);
  });
  onDestroy(() => {
    off?.();
    removeEventListener('keydown', keys);
  });

  function setView(v) {
    prefs.view = v;
    store('hopperd-files-view', v);
  }
  function flipPane(k) {
    prefs[k] = !prefs[k];
    store(`hopperd-files-${k}`, prefs[k] ? '1' : '0');
  }

  async function searchIn(ev) {
    ev?.preventDefault();
    if (filter.trim().length < 2) return;
    finding = true;
    try {
      found = await api(`${base}/search?path=${encodeURIComponent(path)}&q=${encodeURIComponent(filter.trim())}${regex ? '&regex=1' : ''}`);
    } catch (e) {
      say(e.message, true);
    } finally {
      finding = false;
    }
  }
  const groups = $derived.by(() => {
    const m = new Map();
    for (const h of found?.hits || []) {
      if (!m.has(h.path)) m.set(h.path, []);
      if (h.line) m.get(h.path).push(h);
    }
    return [...m];
  });
  /** Focuses the path box when it opens. */
  const focus = (node) => {
    node.focus();
    node.select();
  };
  const openMods = () => (location.hash = `/server/${encodeURIComponent(server.id)}/mods`);
</script>

<svelte:window onclick={() => (ctx = null)} onmouseup={bandEnd} onmousemove={bandMove} />

<!-- The whole explorer is a drop zone (uploads) and listens for mouse back/forward. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="fx"
  role="application"
  aria-label="Files"
  onmousedown={mouseNav}
  onmouseup={mouseNav}
  ondragover={(e) => { e.preventDefault(); if (e.dataTransfer.types.includes('Files') && !e.dataTransfer.types.includes(INTERNAL)) dragging = true; }}
  ondragleave={(e) => e.currentTarget.contains(e.relatedTarget) || (dragging = false)}
  ondrop={(e) => dropOn(e, path)}
>
  <div class="ftool">
    <button class="btn sm icon" title="Back (Alt+←, mouse back)" disabled={!canBack} onclick={back}><ArrowLeft size={14} /></button>
    <button class="btn sm icon" title="Forward (Alt+→, mouse forward)" disabled={!canFwd} onclick={forward}><ArrowRight size={14} /></button>
    <button class="btn sm icon" title="Up (Alt+↑, Backspace)" disabled={!path} onclick={up}><ArrowUp size={14} /></button>
    {#if editingPath}
      <form class="input addr" onsubmit={submitPath}><HardDrive size={13} /><input bind:value={pathText} placeholder="config/ae2" onblur={() => (editingPath = false)} onkeydown={(e) => e.key === 'Escape' && (editingPath = false)} use:focus /></form>
    {:else}
      <div class="addr crumbs" role="button" tabindex="0" title="Click to type a path" onclick={(e) => e.target === e.currentTarget && startEditPath()} onkeydown={(e) => e.key === 'Enter' && startEditPath()}>
        <button class="crumb" onclick={() => navTo('')} ondragover={(e) => { e.preventDefault(); }} ondrop={(e) => dropOn(e, '')}><HardDrive size={13} />{server.name}</button>
        {#each crumbs as c, i (i)}
          <span class="sep">›</span>
          <button class="crumb" class:cur={i === crumbs.length - 1} onclick={() => navTo(crumbs.slice(0, i + 1).join('/'))} ondragover={(e) => { e.preventDefault(); }} ondrop={(e) => dropOn(e, crumbs.slice(0, i + 1).join('/'))}>{c}</button>
        {/each}
        <span class="sp" role="presentation" onclick={startEditPath}></span>
      </div>
    {/if}
    <form class="input find" onsubmit={searchIn} title="Type to filter; Enter searches inside files here and below">
      <Search size={13} /><input placeholder="Filter · Enter searches inside" bind:value={filter} />
      <button type="button" class="rx" class:on={regex} title="Regular expression" onclick={() => (regex = !regex)}>.*</button>
    </form>
    <button class="btn sm icon" title="Search inside files here and below" disabled={filter.trim().length < 2 || finding} onclick={searchIn}><TextSearch size={14} /></button>
    <div class="seg">
      <button class:on={prefs.view === 'list'} title="List" onclick={() => setView('list')}><List size={14} /></button>
      <button class:on={prefs.view === 'tiles'} title="Tiles" onclick={() => setView('tiles')}><LayoutGrid size={14} /></button>
    </div>
    {#if inConfig && prefs.view === 'list'}<button class="btn sm icon" class:on={prefs.group} title="Group by mod" onclick={() => flipPane('group')}><Layers size={14} /></button>{/if}
    <button class="btn sm icon" class:on={prefs.tree} title="Folder tree" onclick={() => flipPane('tree')}><PanelLeft size={14} /></button>
    <button class="btn sm icon" class:on={prefs.preview} title="Preview pane" onclick={() => flipPane('preview')}><PanelRight size={14} /></button>
    <button class="btn sm icon" title="Refresh (F5)" onclick={refresh}><RefreshCw size={14} /></button>
    <button class="btn sm icon" title="New file" onclick={newFile}><FilePlus size={14} /></button>
    <button class="btn sm icon" title="New folder" onclick={mkdir}><FolderPlus size={14} /></button>
    <button class="btn sm icon" title="Upload a folder" onclick={() => folderPick.click()}><FolderUp size={14} /></button>
    <button class="btn sm pri" onclick={() => filePick.click()}><Upload size={14} />Upload</button>
    <button class="btn sm icon ghost" title="Trash" onclick={openTrash}><Trash2 size={14} /></button>
    <input type="file" multiple hidden bind:this={filePick} onchange={(e) => picked(e, false)} />
    <input type="file" hidden webkitdirectory bind:this={folderPick} onchange={(e) => picked(e, true)} />
  </div>

  <div class="body">
    {#if prefs.tree}
      <div class="treepane"><FileTree {base} rootName={server.name} current={path} go={navTo} onDrop={dropOn} version={treeVersion} /></div>
    {/if}

    <div class="main">
      {#if found || finding}
        <div class="found">
          <div class="fh">{#if finding}<span class="spinner muted"></span>{/if}<span class="small">{finding ? 'Searching…' : `${found.hits.length}${found.more ? '+' : ''} matches in ${groups.length} file${groups.length === 1 ? '' : 's'}`}</span><span class="sp"></span>
            <button class="btn sm icon ghost" title="Close search" onclick={() => (found = null)}><X size={13} /></button></div>
          {#if found}
            {#each groups as [p, hits] (p)}
              {@const hm = ownerOf(owners, p)}
              <div class="fg">
                <div class="fpr">
                  {#if hm}<button class="modic" title="Show {hm.name}" onclick={() => showMod(hm)}>{#if hm.icon}<img src={hm.icon} alt="" />{:else}<Package size={13} />{/if}</button>{/if}
                  <button class="fp mono small" onclick={() => openHit(p)}>{p}</button>
                  {#if hm}<button class="tiny muted modn" onclick={() => showMod(hm)}>{hm.name}</button>{/if}
                </div>
                {#each hits.slice(0, 20) as h (h.line)}
                  <button class="hit mono tiny" onclick={() => openHit(p, h.line)}><span class="muted ln">{h.line}</span>{h.text}</button>
                {/each}
                {#if hits.length > 20}<div class="tiny muted ln2">and {hits.length - 20} more</div>{/if}
              </div>
            {:else}
              <div class="empty-state"><span class="es-ic"><TextSearch size={18} /></span><b>Nothing found</b><p>No file names or lines here and below match. Try fewer words, or the .* switch for a pattern.</p></div>
            {/each}
          {/if}
        </div>
      {:else}
        {#if prefs.view === 'list'}
          <div class="head row"><span class="c-name">Name</span><span class="c-date">Changed</span><span class="c-kind">Type</span><span class="c-size">Size</span></div>
        {/if}
        <div
          class="list"
          class:tilesv={prefs.view === 'tiles'}
          bind:this={scroller}
          bind:clientHeight={viewH}
          onscroll={() => (scrollTop = scroller.scrollTop)}
          onmousedown={bandStart}
          oncontextmenu={(e) => e.target === e.currentTarget || e.target.classList.contains('spacer') || e.target.classList.contains('tiles') ? menu(e) : null}
          role="grid"
          tabindex="-1"
        >
          {#if error}
            <div class="empty-state"><span class="es-ic"><TriangleAlert size={18} /></span><b>Can't open this folder</b><p class="err">{error}</p></div>
          {:else if loading && !entries.length}
            <div class="skels">{#each [38, 52, 30, 46, 60, 34, 44, 28] as w, i (i)}<div class="skel"><i style:width="{w}%"></i></div>{/each}</div>
          {:else if !loading && !shown.length}
            {#if filter}
              <div class="empty-state"><span class="es-ic"><Search size={18} /></span><b>Nothing matches</b><p>Press Enter to search inside the files here and below.</p></div>
            {:else}
              <div class="empty-state"><span class="es-ic"><FolderOpen size={18} /></span><b>This folder is empty</b><p>Drop files here to upload them, or make a new file or folder.</p></div>
            {/if}
          {/if}
          {#if prefs.view === 'list'}
            <div style:height="{items.length * ROW}px" class="spacer" data-pick-list="files">
              {#each slice as e, i (e.key ?? e.name)}
                {#if e.key != null}
                  <div class="frow ghead" style:top="{(first + i) * ROW}px" role="row">
                    {#if e.head}
                      <button class="gh" title="Show {e.head.name}" onclick={() => showMod(e.head)}>{#if e.head.icon}<img src={e.head.icon} alt="" />{:else}<span class="ph"><Package size={12} /></span>{/if}<b class="nm">{e.head.name}</b></button>
                    {:else}
                      <span class="gh"><span class="ph"></span><b class="nm muted">Not a mod's settings</b></span>
                    {/if}
                    <span class="tiny muted">{e.n}</span>
                  </div>
                {:else}
                {@const Icon = icon(e)}
                {@const m = inConfig && !grouped ? modOf(e) : null}
                <div
                  class="frow row-hover"
                  class:row-sel={selected.has(e.name)}
                  class:cut={clip.op === 'move' && clip.sid === sid && clip.paths.includes(join(e.name))}
                  class:over={overDir === e.name}
                  style:top="{(first + i) * ROW}px"
                  data-name={e.name}
                  data-pick-ix={first + i}
                  role="row"
                  tabindex="-1"
                  onclick={(ev) => click(e, ev)}
                  ondblclick={() => open(e)}
                  oncontextmenu={(ev) => { ev.stopPropagation(); menu(ev, e); }}
                  ondragover={(ev) => dirOver(ev, e)}
                  ondragleave={() => overDir === e.name && (overDir = null)}
                  ondrop={(ev) => e.dir && dropOn(ev, join(e.name))}
                  onkeydown={() => {}}
                >
                  <input type="checkbox" checked={selected.has(e.name)} onpointerdown={(ev) => tickDown(ev, e, first + i)} onclick={(ev) => { ev.stopPropagation(); if (clickSwallowed()) { ev.preventDefault(); return; } toggle(e); }} />
                  <span class="c-name"><span class="dragh" draggable="true" ondragstart={(ev) => dragStart(ev, e)} role="presentation"><Icon size={15} class={e.dir ? 'fold' : 'fic'} /><span class="nm">{e.name}</span></span>
                    {#if m}<button class="modtag" title="Show {m.name}" onclick={(ev) => { ev.stopPropagation(); showMod(m); }}>{#if m.icon}<img src={m.icon} alt="" />{/if}<span class="ell">{m.name}</span></button>{/if}</span>
                  <span class="c-date muted">{when(e.modified)}</span>
                  <span class="c-kind muted">{kind(e)}</span>
                  <span class="c-size muted">{e.dir ? '' : bytes(e.size)}</span>
                </div>
                {/if}
              {/each}
            </div>
          {:else}
            <div class="tiles">
              {#each shown as e (e.name)}
                {@const Icon = icon(e)}
                <div
                  class="tile row-hover"
                  class:sel={selected.has(e.name)}
                  class:cut={clip.op === 'move' && clip.sid === sid && clip.paths.includes(join(e.name))}
                  class:over={overDir === e.name}
                  data-name={e.name}
                  role="gridcell"
                  tabindex="-1"
                  onclick={(ev) => click(e, ev)}
                  ondblclick={() => open(e)}
                  oncontextmenu={(ev) => { ev.stopPropagation(); menu(ev, e); }}
                  ondragover={(ev) => dirOver(ev, e)}
                  ondragleave={() => overDir === e.name && (overDir = null)}
                  ondrop={(ev) => e.dir && dropOn(ev, join(e.name))}
                  onkeydown={() => {}}
                >
                  <span class="dragh big" draggable="true" ondragstart={(ev) => dragStart(ev, e)} role="presentation">
                    {#if IMAGE.test(e.name) && e.size < 4 * 1024 * 1024}<img src={fileUrl(join(e.name), true)} alt="" loading="lazy" />{:else}<Icon size={30} class={e.dir ? 'fold' : 'fic'} />{/if}
                  </span>
                  <span class="tn">{e.name}</span>
                </div>
              {/each}
            </div>
          {/if}
          {#if band?.live}
            <div class="band" style:left="{Math.min(band.x0, band.x1)}px" style:top="{Math.min(band.y0, band.y1)}px" style:width="{Math.abs(band.x1 - band.x0)}px" style:height="{Math.abs(band.y1 - band.y0)}px"></div>
          {/if}
        </div>
      {/if}

      {#if selList.length && !found}
        <div class="selbar">
          <b>{selList.length} selected</b><span class="muted small">{selSize ? `· ${bytes(selSize)}` : ''}</span>
          <span class="sp"></span>
          <button class="btn sm" onclick={() => downloadSel()}><Download size={13} />{selList.length === 1 && !selList[0].dir ? 'Download' : 'Download zip'}</button>
          {#if selList.length === 1}<button class="btn sm" onclick={rename}><Pencil size={13} />Rename</button>{/if}
          <button class="btn sm" onclick={() => moveOrCopy('copy')}><Copy size={13} />Copy to…</button>
          <button class="btn sm" onclick={() => moveOrCopy('move')}><Scissors size={13} />Move to…</button>
          <button class="btn sm bad" onclick={trash}><Trash2 size={13} />Trash</button>
        </div>
      {/if}
    </div>

    {#if peekMod}
      <div class="peekpane"><ModPeek {server} mod={peekMod} onClose={() => (peekJar = '')} onChanged={async () => (owners = await configOwners(sid, true))} onUpdate={openMods} /></div>
    {:else if prefs.preview}
      <aside class="prev">
        {#if preview}
          {@const Icon = icon(preview.e)}
          <div class="pv-h"><Icon size={18} class={preview.e.dir ? 'fold' : 'fic'} /><b class="ell">{preview.e.name}</b></div>
          <div class="tiny muted">{kind(preview.e)}{preview.e.dir ? '' : ` · ${bytes(preview.e.size)}`}{preview.e.modified ? ` · ${new Date(preview.e.modified * 1000).toLocaleString()}` : ''}</div>
          {@const pm = ownerOf(owners, preview.p)}
          {#if pm}<button class="modtag big" title="Show {pm.name}" onclick={() => showMod(pm)}>{#if pm.icon}<img src={pm.icon} alt="" />{:else}<Package size={13} />{/if}<span class="ell">{pm.name}'s config</span></button>{/if}
          <div class="row wrap">
            <button class="btn sm" onclick={() => open(preview.e)}><ExternalLink size={12} />Open</button>
            {#if !preview.e.dir}<button class="btn sm ghost" onclick={() => downloadSel([preview.e])}><Download size={12} />Download</button>{/if}
            {#if /\.jar$/i.test(preview.e.name) && path.startsWith('mods')}<button class="btn sm ghost" onclick={openMods}><Package size={12} />Mods tab</button>{/if}
          </div>
          {#if IMAGE.test(preview.e.name)}
            <img class="pimg" src={fileUrl(preview.p, true)} alt="" />
          {:else if preview.text != null}
            <pre class="mono">{preview.text}</pre>
          {:else if preview.zip}
            {#if preview.zip.mod?.name}<div class="small">Mod: <b>{preview.zip.mod.name}</b>{preview.zip.mod.ids?.length ? ` · ${preview.zip.mod.ids.join(', ')}` : ''}</div>{/if}
            <div class="tiny muted">{preview.zip.total} entries · double-click to look inside</div>
            <div class="zlist">{#each preview.zip.entries.slice(0, 40) as z (z.name)}<div class="tiny ell">{z.dir ? '📁' : '·'} {z.name}</div>{/each}</div>
          {/if}
        {:else if selList.length > 1}
          <div class="pv-h"><b>{selList.length} selected</b></div>
          <div class="tiny muted">{bytes(selSize)} in files</div>
          <div class="row wrap"><button class="btn sm" onclick={() => downloadSel()}><Download size={12} />Download zip</button><button class="btn sm ghost" onclick={() => showProps(selList)}><Info size={12} />Properties</button></div>
        {:else}
          <div class="small muted">Select a file to preview it.</div>
          <div class="tiny muted keys">Double-click opens · Right-click for more · Drag onto a folder to move (Ctrl copies) · Ctrl+X / C / V · F2 renames · Del trashes · Alt+← → go back and forward</div>
        {/if}
      </aside>
    {/if}
  </div>

  {#if dragging}
    <div class="drop"><div><FolderOpen size={28} /><div>Drop to upload into <b>{path || server.name}</b></div><div class="tiny muted">or onto a folder to upload into it</div></div></div>
  {/if}
</div>

{#if ctx}
  <div class="popover menu ctx" style:left="{ctx.x}px" style:top="{ctx.y}px" role="menu" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
    {#if ctx.e}
      <button class="menu-item" role="menuitem" onclick={() => act(() => open(ctx.e))}><ExternalLink size={13} /><span class="sp">Open</span><kbd>Enter</kbd></button>
      {#if !ctx.e.dir && (TEXT.test(ctx.e.name) || ctx.e.size < 64 * 1024)}<button class="menu-item" role="menuitem" onclick={() => act(() => openFile(join(ctx.e.name), ctx.e.size))}><FileText size={13} /><span class="sp">Edit as text</span></button>{/if}
      <button class="menu-item" role="menuitem" onclick={() => act(() => downloadSel())}><Download size={13} /><span class="sp">{selList.length === 1 && !ctx.e.dir ? 'Download' : 'Download as zip'}</span></button>
      {#if /\.jar$/i.test(ctx.e.name) && path.startsWith('mods')}<button class="menu-item" role="menuitem" onclick={() => act(openMods)}><Package size={13} /><span class="sp">Show in Mods tab</span></button>{/if}
      <div class="menu-sep"></div>
      <button class="menu-item" role="menuitem" onclick={() => act(() => cutCopy('move'))}><Scissors size={13} /><span class="sp">Cut</span><kbd>Ctrl+X</kbd></button>
      <button class="menu-item" role="menuitem" onclick={() => act(() => cutCopy('copy'))}><Copy size={13} /><span class="sp">Copy</span><kbd>Ctrl+C</kbd></button>
      {#if ctx.e.dir && clip.paths.length && clip.sid === sid}<button class="menu-item" role="menuitem" onclick={() => act(() => paste(join(ctx.e.name)))}><ClipboardPaste size={13} /><span class="sp ell">Paste into {ctx.e.name}</span></button>{/if}
      {#if selList.length === 1}<button class="menu-item" role="menuitem" onclick={() => act(rename)}><Pencil size={13} /><span class="sp">Rename</span><kbd>F2</kbd></button>{/if}
      <button class="menu-item bad" role="menuitem" onclick={() => act(trash)}><Trash2 size={13} /><span class="sp">Move to trash</span><kbd>Del</kbd></button>
      <div class="menu-sep"></div>
      <button class="menu-item" role="menuitem" onclick={() => act(() => copyPath(join(ctx.e.name)))}><Link size={13} /><span class="sp">Copy path</span></button>
      <button class="menu-item" role="menuitem" onclick={() => act(() => showProps(selList))}><Info size={13} /><span class="sp">Properties</span><kbd>Alt+Enter</kbd></button>
    {:else}
      <button class="menu-item" role="menuitem" disabled={!clip.paths.length || clip.sid !== sid} onclick={() => act(() => paste())}><ClipboardPaste size={13} /><span class="sp">Paste{clip.paths.length && clip.sid === sid ? ` ${clip.paths.length}` : ''}</span><kbd>Ctrl+V</kbd></button>
      <div class="menu-sep"></div>
      <button class="menu-item" role="menuitem" onclick={() => act(newFile)}><FilePlus size={13} /><span class="sp">New file</span></button>
      <button class="menu-item" role="menuitem" onclick={() => act(mkdir)}><FolderPlus size={13} /><span class="sp">New folder</span></button>
      <button class="menu-item" role="menuitem" onclick={() => act(() => filePick.click())}><Upload size={13} /><span class="sp">Upload files</span></button>
      <div class="menu-sep"></div>
      <button class="menu-item" role="menuitem" onclick={() => act(refresh)}><RefreshCw size={13} /><span class="sp">Refresh</span><kbd>F5</kbd></button>
      <button class="menu-item" role="menuitem" onclick={() => act(() => copyPath(path || '/'))}><Link size={13} /><span class="sp">Copy this folder's path</span></button>
    {/if}
  </div>
{/if}

{#if props}
  <div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && (props = null)}>
    <div class="pbox popover col">
      <h3 class="row"><Info size={14} />{props.title}<span class="sp"></span><button class="btn sm ghost icon" title="Close (Esc)" onclick={() => (props = null)}><X size={14} /></button></h3>
      <div class="kv">{#each props.rows as [k, v] (k)}<span class="muted">{k}</span><span class="mono small sel-t">{v}</span>{/each}</div>
    </div>
  </div>
{/if}

{#if viewer}<FileViewer {base} {sid} mode={viewer.mode} path={viewer.path} images={viewer.images || []} onClose={() => (viewer = null)} {openMods} />{/if}

<style>
  .fx { position: relative; display: flex; flex-direction: column; flex: 1; min-height: 0; border: 1px solid var(--line); border-radius: 10px; overflow: hidden; background: var(--base); }
  .ftool { display: flex; align-items: center; gap: 6px; padding: 8px 10px; border-bottom: 1px solid var(--line); flex-wrap: wrap; }
  .addr { flex: 1; min-width: 220px; display: flex; align-items: center; gap: 2px; border: 1px solid var(--line2); border-radius: 6px; padding: 2px 6px; background: var(--bg); cursor: text; min-height: 30px; }
  .addr.input { padding: 3px 8px; }
  .crumbs { color: var(--subtle); font-size: 12.5px; overflow: hidden; }
  .crumb { display: inline-flex; align-items: center; gap: 5px; padding: 2px 5px; border-radius: 5px; white-space: nowrap; cursor: pointer; }
  .crumb:hover { background: var(--raise); color: var(--text); }
  .crumb.cur { color: var(--text); font-weight: 600; }
  .sep { color: var(--muted); }
  .find { padding: 3px 8px; width: 210px; }
  .find .rx { font: 600 11px var(--mono); color: var(--muted); padding: 0 3px; border-radius: 4px; }
  .find .rx.on { color: var(--acc); background: var(--acc-dim); }
  .seg { display: flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; }
  .seg button { padding: 4px 7px; color: var(--subtle); }
  .seg button.on { background: var(--raise); color: var(--text); }
  .btn.on { background: var(--raise); color: var(--text); }
  .body { flex: 1; min-height: 0; display: flex; }
  .treepane { width: 220px; flex: none; border-right: 1px solid var(--line); overflow: auto; }
  .main { flex: 1; min-width: 0; display: flex; flex-direction: column; }
  .found { flex: 1; overflow: auto; min-height: 200px; }
  .fh { display: flex; align-items: center; gap: 8px; padding: 6px 12px; border-bottom: 1px solid var(--line); position: sticky; top: 0; background: var(--base); }
  .fg { padding: 6px 0; border-bottom: 1px solid var(--line); }
  .fp { display: block; padding: 2px 12px; color: var(--acc); text-align: left; }
  .hit { display: flex; gap: 10px; width: 100%; text-align: left; padding: 1px 12px 1px 24px; white-space: pre; overflow: hidden; text-overflow: ellipsis; color: var(--subtle); }
  .hit:hover, .fp:hover { background: var(--hover); }
  .ln { min-width: 34px; text-align: right; flex: none; }
  .ln2 { padding-left: 24px; }
  .head { padding: 6px 12px 6px 40px; border-bottom: 1px solid var(--line); color: var(--muted); font-size: 11.5px; font-weight: 500; }
  .list { flex: 1; overflow: auto; position: relative; min-height: 200px; user-select: none; }
  .spacer { position: relative; }
  .frow { position: absolute; left: 0; right: 0; height: 34px; display: flex; align-items: center; gap: 8px; padding: 0 12px; border-bottom: 1px solid #26262c; cursor: default; }
  .tile.sel, .tile.sel:hover { background: var(--sel); box-shadow: inset 0 0 0 1px #f9731655; }
  .ghead { background: var(--base); gap: 8px; }
  .gh { display: inline-flex; align-items: center; gap: 8px; min-width: 0; padding: 2px 6px 2px 2px; border-radius: 5px; font-size: 12.5px; }
  button.gh:hover { background: var(--hover); }
  .gh img, .gh .ph { width: 18px; height: 18px; border-radius: 4px; flex: none; background: var(--raise); display: grid; place-items: center; color: var(--muted); }
  .modtag { display: inline-flex; align-items: center; gap: 5px; margin-left: 10px; padding: 1px 7px 1px 3px; border-radius: 99px; background: #ffffff0d; color: var(--subtle); font-size: 11px; max-width: 200px; min-width: 0; flex: 0 1 auto; }
  .modtag:hover { background: #ffffff1a; color: var(--text); }
  .modtag img { width: 14px; height: 14px; border-radius: 3px; flex: none; }
  .modtag.big { margin: 0; align-self: flex-start; font-size: 12px; padding: 3px 9px 3px 4px; max-width: 100%; }
  .modtag.big img { width: 18px; height: 18px; }
  .fpr { display: flex; align-items: center; gap: 6px; padding-left: 12px; }
  .fpr .fp { padding-left: 0; }
  .modic { display: grid; place-items: center; width: 16px; height: 16px; color: var(--muted); }
  .modic img { width: 16px; height: 16px; border-radius: 3px; }
  .modn:hover { color: var(--text); }
  .peekpane { flex: none; border-left: 1px solid var(--line); padding: 8px; overflow: auto; }
  .peekpane :global(.peek) { width: 340px; max-height: none; position: static; }
  .skels { display: flex; flex-direction: column; padding: 0 4px 0 32px; }
  .frow.over, .tile.over { background: var(--sel); outline: 1px dashed var(--acc); outline-offset: -2px; }
  .frow.cut, .tile.cut { opacity: 0.5; }
  .frow input { accent-color: var(--acc); }
  .c-name { flex: 1; min-width: 0; display: flex; align-items: center; }
  .dragh { display: inline-flex; align-items: center; gap: 8px; min-width: 0; max-width: 100%; cursor: grab; }
  .c-size { width: 86px; text-align: right; }
  .c-date { width: 110px; text-align: right; }
  .c-kind { width: 70px; text-align: right; }
  .nm { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .list :global(.fold) { color: var(--acc); flex: none; }
  .list :global(.fic) { color: var(--muted); flex: none; }
  .tiles { display: grid; grid-template-columns: repeat(auto-fill, minmax(104px, 1fr)); gap: 6px; padding: 10px; }
  .tile { display: flex; flex-direction: column; align-items: center; gap: 6px; padding: 10px 6px 8px; border-radius: 8px; text-align: center; cursor: default; }
  .dragh.big { width: 56px; height: 56px; justify-content: center; border-radius: 8px; overflow: hidden; }
  .dragh.big img { width: 56px; height: 56px; object-fit: contain; image-rendering: pixelated; background: var(--raise); border-radius: 6px; }
  .tn { font-size: 11.5px; word-break: break-word; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .band { position: absolute; border: 1px solid var(--acc); background: #f9731622; pointer-events: none; z-index: 3; }
  .selbar { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-top: 1px solid var(--line); background: var(--surface); flex-wrap: wrap; }
  .prev { width: 280px; flex: none; border-left: 1px solid var(--line); padding: 10px 12px; display: flex; flex-direction: column; gap: 8px; overflow: auto; }
  .pv-h { display: flex; align-items: center; gap: 8px; }
  .prev :global(.fold) { color: var(--acc); }
  .prev :global(.fic) { color: var(--muted); }
  .prev pre { margin: 0; background: var(--term); border: 1px solid var(--line); border-radius: 6px; padding: 8px; font-size: 11px; white-space: pre-wrap; word-break: break-all; max-height: 420px; overflow: auto; }
  .pimg { max-width: 100%; image-rendering: pixelated; background: var(--raise); border-radius: 6px; }
  .zlist { display: flex; flex-direction: column; gap: 1px; max-height: 360px; overflow: auto; }
  .keys { line-height: 1.6; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .row.wrap { flex-wrap: wrap; }
  .drop { position: absolute; inset: 0; border: 2px dashed var(--acc); border-radius: 10px; background: #f9731614; display: grid; place-items: center; text-align: center; color: var(--text); pointer-events: none; }
  .drop > div { display: flex; flex-direction: column; align-items: center; gap: 8px; background: var(--surface); padding: 16px 22px; border-radius: 10px; }
  .ctx { position: fixed; z-index: 80; width: 240px; }
  .ctx .menu-item:disabled { opacity: 0.45; cursor: default; background: none; }
  .ctx .menu-item.bad { color: var(--red); }
  .ctx kbd { color: var(--muted); font: 11px var(--mono); }
  .pbox { width: min(480px, 100%); gap: 10px; padding: 16px 18px; }
  .pbox h3 { margin: 0; font-size: 14px; font-weight: 600; gap: 7px; }
  .kv { display: grid; grid-template-columns: 120px 1fr; gap: 6px 12px; }
  .sel-t { user-select: text; word-break: break-all; }
  .icon { padding: 4px 6px; }
  @media (max-width: 1100px) { .prev { display: none; } }
  @media (max-width: 860px) { .treepane { display: none; } .c-date, .c-kind { display: none; } .find { width: 100%; order: 10; } }
</style>
