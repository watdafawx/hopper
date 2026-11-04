<script>
  // One log file (latest.log, debug.log) in Crashes & logs: its last part,
  // then what the server writes to it, read every 2 s while it runs and
  // the page is in view. Search, "Errors and warnings only", lines coloured
  // by level (desktop's hopper::logs::level). Only the lines in view are
  // drawn, so a long log stays quick.
  import { tick, onDestroy } from 'svelte';
  import { Search, X, Share2, FileText, ArrowDown, ScrollText } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { rightClear } from './lib/rightclear.js';

  let { server, path, onShare, openFile } = $props();

  const KEEP = 200000;
  const TAIL = 400 << 10;
  const ROW = 19;
  const name = $derived(path.split('/').pop());
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));

  let lines = $state.raw([]);
  let to = 0;
  let loading = $state(true);
  let missing = $state(false);
  let err = $state('');
  let cut = $state(false);
  let query = $state('');
  let onlyBad = $state(false);
  let visible = $state(document.visibilityState === 'visible');
  // Only the rows in view (and a few around) are drawn.
  let box = $state();
  let top = $state(0);
  let height = $state(600);
  let stuck = $state(true);
  // Bumped when the file changes: a read still out is dropped.
  let gen = 0;
  let reading = false;

  // Error 'e', warning 'w', or '' (a stack trace line takes its error's level).
  const levelOf = (l) => (/ERROR|FATAL/.test(l) ? 'e' : /WARN/.test(l) ? 'w' : '');
  const trace = (l) => /^\s+at |^\s*Caused by:|^\s*\.\.\. \d+ more/.test(l);
  let levels = $state.raw([]);
  function levelsFor(add, prev) {
    const out = new Array(add.length);
    for (let i = 0; i < add.length; i++) {
      const l = levelOf(add[i]);
      out[i] = l || (trace(add[i]) ? (i ? out[i - 1] : prev) : '');
    }
    return out;
  }

  async function read() {
    if (reading) return;
    reading = true;
    const g = gen;
    try {
      const r = await api(`${base}/logs/text?path=${encodeURIComponent(path)}&from=${to}`);
      if (g !== gen) return;
      err = '';
      missing = false;
      const add = r.text ? r.text.replace(/\r?\n$/, '').split(/\r?\n/) : [];
      if (r.reset) {
        lines = add;
        levels = levelsFor(add, '');
        // A first read is the file's last 400 KB.
        cut = r.size > TAIL;
      } else if (add.length) {
        levels = levels.concat(levelsFor(add, levels[levels.length - 1] || ''));
        lines = lines.concat(add);
      }
      if (lines.length > KEEP) {
        const drop = lines.length - KEEP;
        lines = lines.slice(drop);
        levels = levels.slice(drop);
        cut = true;
      }
      to = r.to;
    } catch (e) {
      if (g !== gen) return;
      // The panel lists only files that exist: not there yet.
      if (/isn't one of this server's logs/.test(e.message)) missing = true;
      else err = e.message;
    } finally {
      if (g === gen) {
        reading = false;
        loading = false;
      }
    }
  }

  // A new file (or server): start over.
  $effect(() => {
    path;
    base;
    gen++;
    reading = false;
    to = 0;
    lines = [];
    levels = [];
    cut = false;
    loading = true;
    stuck = true;
    read();
  });

  // Follow it while the server runs and the page is in view.
  $effect(() => {
    if (!alive || !visible || path.endsWith('.gz')) return;
    read();
    const t = setInterval(read, 2000);
    return () => clearInterval(t);
  });

  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q && !onlyBad) return null;
    const out = [];
    for (let i = 0; i < lines.length; i++) {
      if (onlyBad && !levels[i]) continue;
      if (q && !lines[i].toLowerCase().includes(q)) continue;
      out.push(i);
    }
    return out;
  });
  const count = $derived(shown ? shown.length : lines.length);

  const first = $derived(Math.max(0, Math.floor(top / ROW) - 30));
  const last = $derived(Math.min(count, Math.ceil((top + height) / ROW) + 30));
  const rows = $derived.by(() => {
    const out = [];
    for (let k = first; k < last; k++) {
      const i = shown ? shown[k] : k;
      out.push({ i, text: lines[i], l: levels[i] });
    }
    return out;
  });

  function onScroll() {
    top = box.scrollTop;
    stuck = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
  }
  function toBottom() {
    stuck = true;
    if (box) box.scrollTop = box.scrollHeight;
  }
  $effect(() => {
    count;
    lines;
    if (stuck) tick().then(() => box && (box.scrollTop = box.scrollHeight));
  });

  onDestroy(() => gen++);
</script>

<svelte:document onvisibilitychange={() => (visible = document.visibilityState === 'visible')} />

<section class="term">
  <div class="tb">
    <div class="input find"><Search size={13} /><input use:rightClear placeholder="Search {name}" bind:value={query} spellcheck="false" />{#if query}<button class="x" title="Clear" onclick={() => (query = '')}><X size={12} /></button>{/if}</div>
    <label class="row small only"><input type="checkbox" bind:checked={onlyBad} />Errors and warnings only</label>
    {#if shown}<span class="tiny muted">{shown.length} of {lines.length} lines</span>{/if}
    <span class="sp"></span>
    {#if alive && !path.endsWith('.gz')}<span class="tiny muted">Follows the server as it writes</span>{/if}
    <button class="btn sm ghost" title="Opens the file in the editor" onclick={() => openFile(path)}><FileText size={13} />Open file</button>
    <button class="btn sm" title="Upload it and get a link for Discord or a bug report. The log becomes public." onclick={() => onShare(path)}><Share2 size={13} />Share on mclo.gs</button>
  </div>
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="lines" bind:this={box} bind:clientHeight={height} onscroll={onScroll} tabindex="0" role="log" aria-label={name}>
    {#if err}<div class="err note">Couldn't read {name}: {err}</div>{/if}
    {#if loading}
      <div class="muted note row"><span class="spinner"></span>Reading {name}…</div>
    {:else if missing || !lines.length}
      <div class="empty-state"><div class="es-ic"><ScrollText size={18} /></div>
        <b>{missing ? `No ${name} yet` : `${name} is empty`}</b>
        <p>{missing ? (name === 'debug.log' ? 'It shows once the server has run. Not every server writes one.' : 'It shows once the server has run.') : 'Lines show here as the server writes them.'}</p></div>
    {:else if !count}
      <div class="muted note">Nothing matches.</div>
    {:else}
      {#if cut}<div class="muted note tiny">The start of the file isn't shown here (it's too long). Open the file or share it for all of it.</div>{/if}
      <div class="inner" style="height: {count * ROW}px; --gw: {Math.max(4, String(lines.length).length) + 2}ch">
        {#each rows as r, k (r.i)}
          <div class="ln {r.l}" class:tr={r.l && !levelOf(r.text)} style="top: {(first + k) * ROW}px" data-n={r.i + 1}>{r.text}</div>
        {/each}
      </div>
    {/if}
  </div>
  {#if !stuck && count}<button class="btn sm jump" onclick={toBottom}><ArrowDown size={13} />Latest</button>{/if}
</section>

<style>
  .term { flex: 1; min-width: 0; min-height: 360px; background: var(--term); border: 1px solid var(--line); border-radius: var(--radius-lg); display: flex; flex-direction: column; overflow: hidden; position: relative; }
  .tb { display: flex; align-items: center; gap: 8px; padding: 7px 10px; border-bottom: 1px solid var(--line); background: var(--base); flex-wrap: wrap; }
  .find { padding: 3px 8px; width: 220px; }
  .find .x { color: var(--muted); display: grid; place-items: center; }
  .only { gap: 6px; color: var(--subtle); white-space: nowrap; }
  .lines { flex: 1; overflow: auto; padding: 10px 12px; font: 12px/19px var(--mono); color: var(--text); outline: none; }
  .lines:focus-visible { box-shadow: inset 0 0 0 1px var(--acc); }
  .lines .empty-state { height: 100%; }
  .note { padding-left: 2px; margin-bottom: 6px; }
  .inner { position: relative; min-width: max-content; }
  .ln { position: absolute; left: 0; height: 19px; padding-left: var(--gw); white-space: pre; }
  /* The number is drawn, not text: picking and copying leave it out. */
  .ln::before { content: attr(data-n); position: absolute; left: 0; width: calc(var(--gw) - 2ch); text-align: right; color: var(--muted); opacity: .6; user-select: none; }
  .ln::selection { background: #f9731659; }
  .ln.e { color: var(--red); } .ln.w { color: var(--gold); }
  .ln.tr { opacity: .75; }
  .jump { position: absolute; right: 16px; bottom: 14px; box-shadow: 0 6px 20px #0008; }
</style>
