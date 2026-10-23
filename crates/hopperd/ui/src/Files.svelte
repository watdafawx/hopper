<script>
  import { onMount, onDestroy } from 'svelte';
  import {
    Folder, FolderOpen, File, FileText, FileArchive, Image, ChevronRight, HardDrive, Search, FolderPlus, Upload, FolderUp,
    Download, Copy, FolderInput, Pencil, Trash2, RefreshCw, ArrowUp, FilePlus,
  } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { say } from './lib/toast.svelte.js';
  import { ask, confirmIt } from './lib/dialog.svelte.js';
  import { queueUploads, onUploaded, download } from './lib/transfers.svelte.js';

  let { server, path = '', go, openFile, openTrash } = $props();

  const ROW = 34;
  const TEXT = /\.(toml|json5?|properties|txt|cfg|conf|ini|ya?ml|js|ts|mcmeta|snbt|log|md|mcfunction|csv|bat|sh|cmd|ps1|xml|zs|nbt\.txt|lang|secret)$/i;
  const ARCHIVE = /\.(zip|jar|gz|tgz|7z|rar|mrpack)$/i;
  const IMAGE = /\.(png|jpe?g|gif|webp)$/i;

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

  const base = $derived(`/fs/${encodeURIComponent(server.id)}`);
  const crumbs = $derived(path ? path.split('/') : []);
  const shown = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    return q ? entries.filter((e) => e.name.toLowerCase().includes(q)) : entries;
  });
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - 10));
  const slice = $derived(shown.slice(first, first + Math.ceil(viewH / ROW) + 20));
  const selList = $derived(entries.filter((e) => selected.has(e.name)));
  const selSize = $derived(selList.reduce((n, e) => n + e.size, 0));

  const join = (name) => (path ? `${path}/${name}` : name);

  async function load() {
    loading = true;
    error = '';
    try {
      const r = await api(`${base}/list?path=${encodeURIComponent(path)}`);
      entries = r.entries;
      selected = new Set([...selected].filter((n) => entries.some((e) => e.name === n)));
    } catch (e) {
      error = e.message;
      entries = [];
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    path;
    server.id;
    selected = new Set();
    filter = '';
    if (scroller) scroller.scrollTop = 0;
    load();
  });

  function icon(e) {
    if (e.dir) return Folder;
    if (TEXT.test(e.name)) return FileText;
    if (ARCHIVE.test(e.name)) return FileArchive;
    if (IMAGE.test(e.name)) return Image;
    return File;
  }

  function when(t) {
    if (!t) return '';
    const d = new Date(t * 1000);
    const age = (Date.now() - d) / 86400000;
    if (age < 1 && d.getDate() === new Date().getDate()) return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    if (age < 7) return `${Math.max(1, Math.round(age))} d ago`;
    return d.toLocaleDateString();
  }

  function click(e, ev) {
    const s = new Set(selected);
    if (ev.shiftKey && last) {
      const a = shown.findIndex((x) => x.name === last);
      const b = shown.findIndex((x) => x.name === e.name);
      for (const x of shown.slice(Math.min(a, b), Math.max(a, b) + 1)) s.add(x.name);
    } else if (ev.ctrlKey || ev.metaKey) {
      s.has(e.name) ? s.delete(e.name) : s.add(e.name);
    } else {
      s.clear();
      s.add(e.name);
    }
    last = e.name;
    selected = s;
  }

  function toggle(e) {
    const s = new Set(selected);
    s.has(e.name) ? s.delete(e.name) : s.add(e.name);
    last = e.name;
    selected = s;
  }

  function open(e) {
    if (e.dir) go(join(e.name));
    else if (TEXT.test(e.name) || e.size < 64 * 1024) openFile(join(e.name), e.size);
    else download(`/api/v1${base}/file?path=${encodeURIComponent(join(e.name))}`);
  }

  async function ops(list, message) {
    try {
      await post(`${base}/ops`, { ops: list });
      if (message) say(message);
    } catch (e) {
      say(e.message, true);
    }
    load();
  }

  async function mkdir() {
    const name = await ask('New folder', '', { ok: 'Create', placeholder: 'Folder name' });
    if (name?.trim()) ops([{ op: 'mkdir', path: join(name.trim()) }]);
  }

  async function newFile() {
    const name = await ask('New file', '', { ok: 'Create', placeholder: 'name.toml' });
    if (!name?.trim()) return;
    try {
      await api(`${base}/text`, { method: 'PUT', body: { path: join(name.trim()), content: '', force: true } });
      openFile(join(name.trim()), 0);
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

  function downloadSel() {
    if (selList.length === 1 && !selList[0].dir) {
      download(`/api/v1${base}/file?path=${encodeURIComponent(join(selList[0].name))}`);
    } else {
      const q = selList.map((e) => `path=${encodeURIComponent(join(e.name))}`).join('&');
      download(`/api/v1${base}/zip?${q}`);
    }
  }

  /** Uploads, asking first if top-level names already exist here. */
  async function upload(items) {
    if (!items.length) return;
    const names = new Set(entries.map((e) => e.name));
    const clash = [...new Set(items.map((i) => i.rel.split('/')[0]))].filter((n) => names.has(n));
    let overwrite = false;
    if (clash.length) {
      overwrite = await confirmIt(`Replace ${clash.length === 1 ? clash[0] : `${clash.length} items`}?`, {
        body: `${clash.slice(0, 5).join(', ')}${clash.length > 5 ? '…' : ''} already exist here. The old copies go to the trash.`,
        ok: 'Replace',
      });
      if (!overwrite) items = items.filter((i) => !clash.includes(i.rel.split('/')[0]));
    }
    queueUploads(server.id, items.map((i) => ({ file: i.file, path: join(i.rel) })), overwrite);
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

  async function drop(ev) {
    ev.preventDefault();
    dragging = false;
    const roots = [...ev.dataTransfer.items].map((i) => i.webkitGetAsEntry?.()).filter(Boolean);
    const out = [];
    for (const r of roots) await readEntry(r, '', out);
    upload(out);
  }

  function keys(ev) {
    if (ev.target.tagName === 'INPUT' || ev.target.tagName === 'TEXTAREA') return;
    if (ev.key === 'Delete' && selList.length) trash();
    else if (ev.key === 'F2' && selList.length === 1) rename();
    else if (ev.key === 'Backspace' && path) go(crumbs.slice(0, -1).join('/'));
    else if ((ev.ctrlKey || ev.metaKey) && ev.key === 'a') {
      ev.preventDefault();
      selected = new Set(shown.map((e) => e.name));
    } else if (ev.key === 'Enter' && selList.length === 1) open(selList[0]);
  }

  let off;
  onMount(() => {
    off = onUploaded((sid, folder) => sid === server.id && folder === path && load());
    addEventListener('keydown', keys);
  });
  onDestroy(() => {
    off?.();
    removeEventListener('keydown', keys);
  });
</script>

<div
  class="wrap"
  role="region"
  aria-label="Files"
  ondragover={(e) => { e.preventDefault(); dragging = true; }}
  ondragleave={(e) => e.currentTarget.contains(e.relatedTarget) || (dragging = false)}
  ondrop={drop}
>
  <div class="ftool">
    <div class="crumbs">
      <button class="crumb" onclick={() => go('')}><HardDrive size={13} />{server.name}</button>
      {#each crumbs as c, i (i)}
        <ChevronRight size={13} />
        <button class="crumb" class:cur={i === crumbs.length - 1} onclick={() => go(crumbs.slice(0, i + 1).join('/'))}>{c}</button>
      {/each}
    </div>
    <span class="tiny muted nw">{entries.length} items</span>
    <span class="sp"></span>
    <div class="input find"><Search size={13} /><input placeholder="Filter this folder" bind:value={filter} /></div>
    <button class="btn sm icon" title="Up a folder" disabled={!path} onclick={() => go(crumbs.slice(0, -1).join('/'))}><ArrowUp size={14} /></button>
    <button class="btn sm icon" title="Refresh" onclick={load}><RefreshCw size={14} /></button>
    <button class="btn sm icon" title="New file" onclick={newFile}><FilePlus size={14} /></button>
    <button class="btn sm icon" title="New folder" onclick={mkdir}><FolderPlus size={14} /></button>
    <button class="btn sm icon" title="Upload a folder" onclick={() => folderPick.click()}><FolderUp size={14} /></button>
    <button class="btn sm pri" onclick={() => filePick.click()}><Upload size={14} />Upload</button>
    <button class="btn sm icon ghost" title="Trash" onclick={openTrash}><Trash2 size={14} /></button>
    <input type="file" multiple hidden bind:this={filePick} onchange={(e) => picked(e, false)} />
    <input type="file" hidden webkitdirectory bind:this={folderPick} onchange={(e) => picked(e, true)} />
  </div>

  <div class="head row">
    <span class="c-name">Name</span><span class="c-size">Size</span><span class="c-date">Changed</span>
  </div>
  <div class="list" bind:this={scroller} bind:clientHeight={viewH} onscroll={() => (scrollTop = scroller.scrollTop)}>
    {#if error}
      <div class="msg err">{error}</div>
    {:else if !loading && !shown.length}
      <div class="msg muted">{filter ? 'Nothing matches.' : 'This folder is empty. Drop files here to upload them.'}</div>
    {/if}
    <div style:height="{shown.length * ROW}px" class="spacer">
      {#each slice as e, i (e.name)}
        {@const Icon = icon(e)}
        <div
          class="frow"
          class:sel={selected.has(e.name)}
          style:top="{(first + i) * ROW}px"
          role="row"
          tabindex="-1"
          onclick={(ev) => click(e, ev)}
          ondblclick={() => open(e)}
          onkeydown={() => {}}
        >
          <input type="checkbox" checked={selected.has(e.name)} onclick={(ev) => { ev.stopPropagation(); toggle(e); }} />
          <span class="c-name"><Icon size={15} class={e.dir ? 'fold' : 'fic'} /><span class="nm">{e.name}</span></span>
          <span class="c-size muted">{e.dir ? '' : bytes(e.size)}</span>
          <span class="c-date muted">{when(e.modified)}</span>
        </div>
      {/each}
    </div>
  </div>

  {#if selList.length}
    <div class="selbar">
      <b>{selList.length} selected</b><span class="muted small">{selSize ? `· ${bytes(selSize)}` : ''}</span>
      <span class="sp"></span>
      <button class="btn sm" onclick={downloadSel}><Download size={13} />{selList.length === 1 && !selList[0].dir ? 'Download' : 'Download zip'}</button>
      {#if selList.length === 1}<button class="btn sm" onclick={rename}><Pencil size={13} />Rename</button>{/if}
      <button class="btn sm" onclick={() => moveOrCopy('copy')}><Copy size={13} />Copy to…</button>
      <button class="btn sm" onclick={() => moveOrCopy('move')}><FolderInput size={13} />Move to…</button>
      <button class="btn sm bad" onclick={trash}><Trash2 size={13} />Trash</button>
    </div>
  {/if}

  {#if dragging}
    <div class="drop"><div><FolderOpen size={28} /><div>Drop to upload into <b>{path || server.name}</b></div></div></div>
  {/if}
</div>

<style>
  .wrap { position: relative; display: flex; flex-direction: column; flex: 1; min-height: 0; border: 1px solid var(--line); border-radius: 10px; overflow: hidden; background: var(--base); }
  .ftool { display: flex; align-items: center; gap: 6px; padding: 8px 10px; border-bottom: 1px solid var(--line); flex-wrap: wrap; }
  .crumbs { display: flex; align-items: center; gap: 2px; color: var(--subtle); font-size: 12.5px; min-width: 0; overflow: hidden; flex-wrap: wrap; }
  .crumb { display: inline-flex; align-items: center; gap: 5px; padding: 2px 5px; border-radius: 5px; white-space: nowrap; }
  .crumb:hover { background: var(--raise); color: var(--text); }
  .crumb.cur { color: var(--text); font-weight: 600; }
  .nw { white-space: nowrap; }
  .find { padding: 3px 8px; width: 180px; }
  .head { padding: 6px 12px 6px 40px; border-bottom: 1px solid var(--line); color: var(--muted); font-size: 11.5px; font-weight: 500; }
  .list { flex: 1; overflow: auto; position: relative; min-height: 200px; }
  .spacer { position: relative; }
  .frow { position: absolute; left: 0; right: 0; height: 34px; display: flex; align-items: center; gap: 8px; padding: 0 12px; border-bottom: 1px solid #26262c; cursor: default; user-select: none; }
  .frow:hover { background: #ffffff06; }
  .frow.sel { background: var(--acc-dim); }
  .frow input { accent-color: var(--acc); }
  .c-name { flex: 1; min-width: 0; display: flex; align-items: center; gap: 8px; }
  .c-size { width: 90px; text-align: right; }
  .c-date { width: 120px; text-align: right; }
  .nm { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .frow :global(.fold) { color: var(--acc); }
  .frow :global(.fic) { color: var(--muted); }
  .msg { padding: 24px; text-align: center; }
  .selbar { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-top: 1px solid var(--line); background: var(--surface); flex-wrap: wrap; }
  .drop { position: absolute; inset: 0; border: 2px dashed var(--acc); border-radius: 10px; background: #f9731614; display: grid; place-items: center; text-align: center; color: var(--text); pointer-events: none; }
  .drop > div { display: flex; flex-direction: column; align-items: center; gap: 8px; background: var(--surface); padding: 16px 22px; border-radius: 10px; }
  @media (max-width: 760px) { .c-date { display: none; } .find { width: 100%; order: 10; } }
</style>
