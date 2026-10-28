<script>
  // Opening files that aren't edited as text: images (with ← → or the mouse
  // wheel through the folder's images, round from the last to the first, as
  // desktop's image viewer) and zips/jars browsed like folders, without
  // extracting.
  import { untrack } from 'svelte';
  import { X, ChevronLeft, ChevronRight, Download, Folder, File, ArrowUp, Package, ExternalLink } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { download } from './lib/transfers.svelte.js';
  import { IMAGE, TEXT } from './lib/files.svelte.js';
  import { dialog } from './lib/dialog.svelte.js';

  let { base, sid, mode, path, images = [], onClose, openMods } = $props();
  let at = $state(untrack(() => Math.max(0, images.indexOf(path))));
  let wheel = 0;
  /** One image on, or back (wrapping round). */
  function step(d) {
    const n = images.length;
    if (n > 1) at = (at + d + n) % n;
  }
  /** The wheel moves like the arrows (down is next): a mouse notch is one
   * step, a touchpad steps every so many pixels (desktop's on_wheel). */
  function onWheel(e) {
    if (images.length < 2) return;
    e.preventDefault();
    if (e.deltaMode !== 0) {
      // Lines or pages (Firefox's mouse wheel): one step a notch.
      wheel = 0;
      if (e.deltaY) step(e.deltaY > 0 ? 1 : -1);
      return;
    }
    wheel += e.deltaY;
    if (Math.abs(wheel) < 60) return;
    step(wheel > 0 ? 1 : -1);
    wheel = 0;
  }
  // The neighbours load while this one shows, ready for the arrows.
  $effect(() => {
    const n = images.length;
    if (mode !== 'image' || n < 2) return;
    for (const d of [1, -1]) new Image().src = src(images[(at + d + n) % n]);
  });
  /** Wheel listener that may cancel the page's scroll (Svelte's onwheel is passive). */
  function wheelable(node) {
    node.addEventListener('wheel', onWheel, { passive: false });
    return { destroy: () => node.removeEventListener('wheel', onWheel) };
  }
  let inner = $state('');
  let zip = $state(null);
  let zerr = $state('');
  let peek = $state(null);

  const cur = $derived(mode === 'image' ? images[at] ?? path : path);
  const src = (p) => `/api/v1${base}/file?path=${encodeURIComponent(p)}&inline=1`;
  const entryUrl = (e, inline = false) => `/api/v1${base}/zip-entry?path=${encodeURIComponent(path)}&entry=${encodeURIComponent(inner ? `${inner}/${e}` : e)}${inline ? '&inline=1' : ''}`;
  const isJar = $derived(/\.jar$/i.test(path));
  const inMods = $derived(path.startsWith('mods/'));

  let zipSeq = 0;
  async function loadZip() {
    const seq = ++zipSeq;
    zerr = '';
    peek = null;
    try {
      const r = await api(`${base}/zip-list?path=${encodeURIComponent(path)}&inner=${encodeURIComponent(inner)}`);
      if (seq === zipSeq) zip = r;
    } catch (e) {
      if (seq === zipSeq) zerr = e.message;
    }
  }

  let peekSeq = 0;
  async function openEntry(e) {
    if (e.dir) {
      inner = inner ? `${inner}/${e.name}` : e.name;
      return;
    }
    if (IMAGE.test(e.name)) {
      peekSeq++;
      peek = { name: e.name, img: entryUrl(e.name, true) };
    } else if (TEXT.test(e.name) || /\.(mcmeta|json|toml|txt|cfg|md|properties|mf|accesswidener)$/i.test(e.name)) {
      const seq = ++peekSeq;
      const t = await fetch(entryUrl(e.name, true), { credentials: 'same-origin' })
        .then((r) => (r.ok ? r.text() : r.json().then((j) => `Couldn't read it: ${j.error || r.status}`, () => `Couldn't read it (HTTP ${r.status}).`)))
        .catch(() => "Couldn't read it: the connection dropped.");
      // Another entry clicked meanwhile wins.
      if (seq === peekSeq) peek = { name: e.name, text: t.slice(0, 200000) };
    } else {
      download(entryUrl(e.name));
    }
  }

  function keys(e) {
    if (dialog.open) return;
    if (e.key === 'Escape') {
      if (peek) peek = null;
      else onClose();
    } else if (mode === 'zip' && e.key === 'Backspace' && inner && !e.target.closest?.('input, textarea')) {
      inner = inner.split('/').slice(0, -1).join('/');
    }
    else if (mode === 'image' && e.key === 'ArrowRight') step(1);
    else if (mode === 'image' && e.key === 'ArrowLeft') step(-1);
  }

  $effect(() => {
    inner;
    if (mode === 'zip') loadZip();
  });
</script>

<svelte:window onkeydown={keys} />

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && onClose()}>
  {#if mode === 'image'}
    <div class="img col">
      <div class="row top"><b class="ell">{cur.split('/').pop()}</b><span class="tiny muted">{images.length > 1 ? `${at + 1} of ${images.length} · ← → or scroll to move` : ''}</span><span class="sp"></span>
        <a class="btn sm ghost icon" title="Open in a new tab" href={src(cur)} target="_blank" rel="noopener"><ExternalLink size={13} /></a>
        <button class="btn sm" onclick={() => download(`/api/v1${base}/file?path=${encodeURIComponent(cur)}`)}><Download size={13} />Download</button>
        <button class="btn sm ghost icon" title="Close (Esc)" onclick={onClose}><X size={14} /></button></div>
      <div class="stage" use:wheelable>
        {#if images.length > 1}<button class="nav l" title="Previous (←)" onclick={() => step(-1)}><ChevronLeft size={22} /></button>{/if}
        <img src={src(cur)} alt="" />
        {#if images.length > 1}<button class="nav r" title="Next (→)" onclick={() => step(1)}><ChevronRight size={22} /></button>{/if}
      </div>
    </div>
  {:else}
    <div class="box popover col">
      <div class="row top"><Package size={15} /><b class="ell">{path.split('/').pop()}</b>
        {#if zip?.mod?.name}<span class="chip">{zip.mod.name}</span>{/if}
        <span class="sp"></span>
        {#if isJar && inMods && openMods}<button class="btn sm" onclick={() => { onClose(); openMods(); }}>Show in Mods tab</button>{/if}
        <button class="btn sm" onclick={() => download(`/api/v1${base}/file?path=${encodeURIComponent(path)}`)}><Download size={13} />Download</button>
        <button class="btn sm ghost icon" onclick={onClose}><X size={14} /></button></div>
      <div class="row small crumbs">
        <button class="btn sm ghost icon" disabled={!inner} onclick={() => (inner = inner.split('/').slice(0, -1).join('/'))} title="Up"><ArrowUp size={13} /></button>
        <span class="mono muted">/{inner}</span><span class="sp"></span>{#if zip}<span class="tiny muted">{zip.total} entries in the archive</span>{/if}
      </div>
      <div class="split2">
        <div class="zl">
          {#if zerr}<div class="err small pad">{zerr}</div>{/if}
          {#each zip?.entries || [] as e (e.name)}
            <button class="ze row-hover" class:row-sel={peek?.name === e.name} onclick={() => openEntry(e)}>
              {#if e.dir}<Folder size={14} class="fo" />{:else}<File size={14} class="fi" />{/if}
              <span class="ell grow">{e.name}</span><span class="tiny muted">{e.dir ? '' : bytes(e.size)}</span>
            </button>
          {:else}
            {#if zip}<div class="empty-state"><span class="es-ic"><Folder size={18} /></span><b>Nothing in here</b></div>{:else if !zerr}{#each [40, 55, 30, 48] as w, i (i)}<div class="skel"><i style:width="{w}%"></i></div>{/each}{/if}
          {/each}
        </div>
        {#if peek}
          <div class="zp">
            <div class="row small"><b class="ell">{peek.name}</b><span class="sp"></span><button class="btn sm ghost" onclick={() => download(entryUrl(peek.name))}><Download size={12} />Download</button></div>
            {#if peek.img}<img src={peek.img} alt="" class="pimg" />{:else}<pre class="mono">{peek.text}</pre>{/if}
          </div>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .top { gap: 8px; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .grow { flex: 1; text-align: left; }
  .icon { padding: 3px 5px; }
  .img { width: min(1100px, 100%); height: calc(100vh - var(--dialog-top) - 24px); max-height: 900px; gap: 8px; color: var(--text); }
  .stage { flex: 1; min-height: 0; display: grid; place-items: center; position: relative; background: repeating-conic-gradient(#1b1b20 0% 25%, #222228 0% 50%) 50% / 24px 24px; border-radius: 10px; overflow: hidden; }
  .stage img { max-width: 100%; max-height: 100%; image-rendering: pixelated; object-fit: contain; min-width: 64px; }
  .nav { position: absolute; top: 50%; transform: translateY(-50%); background: #000a; border-radius: 99px; padding: 8px; color: #fff; }
  .nav.l { left: 10px; } .nav.r { right: 10px; }
  .box { width: min(1000px, 100%); height: min(calc(100vh - var(--dialog-top) - 24px), 760px); gap: 8px; padding: 14px; }
  .crumbs { gap: 6px; }
  .split2 { flex: 1; min-height: 0; display: flex; gap: 10px; }
  .zl { flex: 1; min-width: 0; overflow: auto; border: 1px solid var(--line); border-radius: 8px; background: var(--base); }
  .ze { display: flex; align-items: center; gap: 8px; width: 100%; padding: 5px 10px; border-bottom: 1px solid #24242a; font-size: 12.5px; }

  .ze :global(.fo) { color: var(--acc); flex: none; }
  .ze :global(.fi) { color: var(--muted); flex: none; }
  .zp { width: 45%; min-width: 260px; display: flex; flex-direction: column; gap: 6px; min-height: 0; }
  .zp pre { flex: 1; overflow: auto; margin: 0; background: var(--term); border: 1px solid var(--line); border-radius: 8px; padding: 8px 10px; font-size: 11.5px; white-space: pre-wrap; }
  .pimg { max-width: 100%; image-rendering: pixelated; background: var(--raise); border-radius: 8px; }
  .pad { padding: 12px; }
</style>
