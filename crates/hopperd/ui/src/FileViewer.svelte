<script>
  // Opening files that aren't edited as text: images (with ← → through the
  // folder's images) and zips/jars browsed like folders, without extracting.
  import { X, ChevronLeft, ChevronRight, Download, Folder, File, ArrowUp, Package } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { download } from './lib/transfers.svelte.js';
  import { IMAGE, TEXT } from './lib/files.svelte.js';

  let { base, sid, mode, path, images = [], onClose, openMods } = $props();
  let at = $state(images.indexOf(path));
  let inner = $state('');
  let zip = $state(null);
  let zerr = $state('');
  let peek = $state(null);

  const cur = $derived(mode === 'image' ? images[at] ?? path : path);
  const src = (p) => `/api/v1${base}/file?path=${encodeURIComponent(p)}&inline=1`;
  const entryUrl = (e, inline = false) => `/api/v1${base}/zip-entry?path=${encodeURIComponent(path)}&entry=${encodeURIComponent(inner ? `${inner}/${e}` : e)}${inline ? '&inline=1' : ''}`;
  const isJar = $derived(/\.jar$/i.test(path));
  const inMods = $derived(path.startsWith('mods/'));

  async function loadZip() {
    zerr = '';
    peek = null;
    try {
      zip = await api(`${base}/zip-list?path=${encodeURIComponent(path)}&inner=${encodeURIComponent(inner)}`);
    } catch (e) {
      zerr = e.message;
    }
  }

  async function openEntry(e) {
    if (e.dir) {
      inner = inner ? `${inner}/${e.name}` : e.name;
      return;
    }
    if (IMAGE.test(e.name)) {
      peek = { name: e.name, img: entryUrl(e.name, true) };
    } else if (TEXT.test(e.name) || /\.(mcmeta|json|toml|txt|cfg|md|properties|mf|accesswidener)$/i.test(e.name)) {
      const t = await fetch(entryUrl(e.name, true), { credentials: 'same-origin' }).then((r) => r.text()).catch(() => '');
      peek = { name: e.name, text: t.slice(0, 200000) };
    } else {
      download(entryUrl(e.name));
    }
  }

  function keys(e) {
    if (e.key === 'Escape') onClose();
    else if (mode === 'image' && e.key === 'ArrowRight' && at < images.length - 1) at++;
    else if (mode === 'image' && e.key === 'ArrowLeft' && at > 0) at--;
  }

  $effect(() => {
    inner;
    if (mode === 'zip') loadZip();
  });
</script>

<svelte:window onkeydown={keys} />

<div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && onClose()}>
  {#if mode === 'image'}
    <div class="img col">
      <div class="row top"><b class="ell">{cur.split('/').pop()}</b><span class="tiny muted">{images.length > 1 ? `${at + 1} of ${images.length}` : ''}</span><span class="sp"></span>
        <button class="btn sm" onclick={() => download(`/api/v1${base}/file?path=${encodeURIComponent(cur)}`)}><Download size={13} />Download</button>
        <button class="btn sm ghost icon" onclick={onClose}><X size={14} /></button></div>
      <div class="stage">
        {#if at > 0}<button class="nav l" onclick={() => at--}><ChevronLeft size={22} /></button>{/if}
        <img src={src(cur)} alt="" />
        {#if at < images.length - 1}<button class="nav r" onclick={() => at++}><ChevronRight size={22} /></button>{/if}
      </div>
    </div>
  {:else}
    <div class="box card col">
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
            <button class="ze" class:on={peek?.name === e.name} ondblclick={() => openEntry(e)} onclick={() => (e.dir ? openEntry(e) : openEntry(e))}>
              {#if e.dir}<Folder size={14} class="fo" />{:else}<File size={14} class="fi" />{/if}
              <span class="ell grow">{e.name}</span><span class="tiny muted">{e.dir ? '' : bytes(e.size)}</span>
            </button>
          {:else}
            {#if zip}<div class="small muted pad">Empty.</div>{:else if !zerr}<div class="small muted pad">Reading…</div>{/if}
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
  .back { position: fixed; inset: 0; background: #000b; display: grid; place-items: center; z-index: 70; padding: 18px; }
  .top { gap: 8px; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .grow { flex: 1; text-align: left; }
  .icon { padding: 3px 5px; }
  .img { width: min(1100px, 100%); height: min(85vh, 900px); gap: 8px; color: var(--text); }
  .stage { flex: 1; min-height: 0; display: grid; place-items: center; position: relative; background: repeating-conic-gradient(#1b1b20 0% 25%, #222228 0% 50%) 50% / 24px 24px; border-radius: 10px; overflow: hidden; }
  .stage img { max-width: 100%; max-height: 100%; image-rendering: pixelated; object-fit: contain; min-width: 64px; }
  .nav { position: absolute; top: 50%; transform: translateY(-50%); background: #000a; border-radius: 99px; padding: 8px; color: #fff; }
  .nav.l { left: 10px; } .nav.r { right: 10px; }
  .box { width: min(1000px, 100%); height: min(80vh, 760px); gap: 8px; box-shadow: 0 24px 60px #000c; }
  .crumbs { gap: 6px; }
  .split2 { flex: 1; min-height: 0; display: flex; gap: 10px; }
  .zl { flex: 1; min-width: 0; overflow: auto; border: 1px solid var(--line); border-radius: 8px; background: var(--base); }
  .ze { display: flex; align-items: center; gap: 8px; width: 100%; padding: 5px 10px; border-bottom: 1px solid #24242a; font-size: 12.5px; }
  .ze:hover { background: #ffffff06; }
  .ze.on { background: var(--acc-dim); }
  .ze :global(.fo) { color: var(--acc); flex: none; }
  .ze :global(.fi) { color: var(--muted); flex: none; }
  .zp { width: 45%; min-width: 260px; display: flex; flex-direction: column; gap: 6px; min-height: 0; }
  .zp pre { flex: 1; overflow: auto; margin: 0; background: var(--term); border: 1px solid var(--line); border-radius: 8px; padding: 8px 10px; font-size: 11.5px; white-space: pre-wrap; }
  .pimg { max-width: 100%; image-rendering: pixelated; background: var(--raise); border-radius: 8px; }
  .pad { padding: 12px; }
</style>
