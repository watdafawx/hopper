<script>
  // The folder tree beside the file list: folders load as they open, the
  // current folder's parents open on their own, and folders take drops.
  import { ChevronRight, ChevronDown, Folder, FolderOpen, HardDrive } from '@lucide/svelte';
  import { untrack } from 'svelte';
  import { api } from './lib/api.js';

  let { base, rootName, current, go, onDrop, version = 0 } = $props();
  /** path -> child folder names (undefined: not loaded) */
  let kids = $state({});
  let open = $state(new Set(['']));
  let over = $state(null);

  async function loadKids(p) {
    try {
      const r = await api(`${base}/list?path=${encodeURIComponent(p)}`);
      kids[p] = r.entries.filter((e) => e.dir).map((e) => e.name);
    } catch {
      kids[p] = [];
    }
  }

  function flip(p) {
    const s = new Set(open);
    if (s.has(p)) s.delete(p);
    else {
      s.add(p);
      if (kids[p] === undefined) loadKids(p);
    }
    open = s;
  }

  // Open the way down to the current folder; reload what's open when the
  // list says something changed.
  let seenVersion = -1;
  $effect(() => {
    const v = version;
    const cur = current;
    untrack(() => {
      const fresh = v !== seenVersion;
      seenVersion = v;
      const parts = cur ? cur.split('/') : [];
      const s = new Set(open);
      let p = '';
      s.add('');
      for (const part of parts) {
        if (kids[p] === undefined) loadKids(p);
        p = p ? `${p}/${part}` : part;
        s.add(p);
      }
      if (kids[cur] === undefined || fresh) loadKids(cur);
      open = s;
    });
  });

  const join = (p, n) => (p ? `${p}/${n}` : n);
  function dragOver(ev, p) {
    ev.preventDefault();
    ev.dataTransfer.dropEffect = ev.ctrlKey ? 'copy' : 'move';
    over = p;
  }
</script>

{#snippet node(p, name, depth)}
  <div
    class="n"
    class:on={current === p}
    class:over={over === p}
    style:padding-left="{6 + depth * 14}px"
    role="treeitem"
    aria-selected={current === p}
    tabindex="-1"
    ondragover={(e) => dragOver(e, p)}
    ondragleave={() => over === p && (over = null)}
    ondrop={(e) => { over = null; onDrop(e, p); }}
  >
    <button class="tw" onclick={() => flip(p)} aria-label="Open or close">{#if kids[p]?.length === 0}<span class="sp0"></span>{:else if open.has(p)}<ChevronDown size={12} />{:else}<ChevronRight size={12} />{/if}</button>
    <button class="lab" onclick={() => go(p)}>{#if p === ''}<HardDrive size={13} />{:else if open.has(p)}<FolderOpen size={13} class="fo" />{:else}<Folder size={13} class="fo" />{/if}<span class="ell">{name}</span></button>
  </div>
  {#if open.has(p) && kids[p]}
    {#each kids[p] as k (k)}{@render node(join(p, k), k, depth + 1)}{/each}
  {/if}
{/snippet}

<nav class="tree" aria-label="Folders">{@render node('', rootName, 0)}</nav>

<style>
  .tree { overflow: auto; padding: 6px 4px; font-size: 12.5px; }
  .n { display: flex; align-items: center; gap: 2px; border-radius: 5px; white-space: nowrap; }
  .n:hover { background: #ffffff06; }
  .n.on { background: var(--raise); box-shadow: inset 3px 0 0 var(--acc); }
  .n.over { background: var(--acc-dim); outline: 1px dashed var(--acc); }
  .tw { width: 16px; height: 22px; display: grid; place-items: center; color: var(--muted); flex: none; }
  .sp0 { width: 12px; }
  .lab { display: flex; align-items: center; gap: 6px; min-width: 0; flex: 1; padding: 3px 4px; text-align: left; color: var(--subtle); }
  .n.on .lab { color: var(--text); }
  .lab :global(.fo) { color: var(--acc); flex: none; }
  .ell { overflow: hidden; text-overflow: ellipsis; }
</style>
