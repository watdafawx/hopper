<script>
  // The server's console, as Hopper desktop's (server_control.rs with
  // line_select.rs): numbered lines whose text can be picked with the mouse
  // (drag, double-click a word, triple-click a line, Ctrl+A, Ctrl+C, a
  // right-click menu). A pick stays put while lines arrive and the view
  // doesn't jump away from it. Live lines or this run's whole latest.log,
  // filters and search, and the command line.
  import { tick, onDestroy } from 'svelte';
  import { CornerDownLeft, Search, ArrowDown, Share2, Copy, TextSelect, X, SquareTerminal } from '@lucide/svelte';
  import { consoles } from './lib/live.svelte.js';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { kind, shift } from './lib/consoleKit.js';
  import { rightClear } from './lib/rightclear.js';

  let { server, onShare } = $props();

  const id = $derived(server.id);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));

  /** 'live' (the panel's latest lines) or 'file' (this run's whole latest.log). */
  let source = $state('live');
  let filter = $state('all');
  let query = $state('');
  let cmd = $state('');
  let hist = [];
  let histAt = -1;
  let box = $state();
  let inner = $state();
  let stuck = $state(true);
  /** The mouse is down on the lines, or text in them is picked. */
  let picking = $state(false);
  let picked = $state(false);
  let menu = $state(null);

  // Live lines keep their keys (and numbers) while the console trims its
  // top: how far the window moved, as desktop's line_select::shift.
  const liveLines = $derived(consoles[id] || []);
  let prevLive = [];
  let liveBase = 0;
  const liveKeyBase = $derived.by(() => {
    const now = liveLines;
    liveBase += shift(prevLive, now);
    prevLive = now;
    return liveBase;
  });

  // The whole log file, read every few seconds while it shows.
  const FILE_KEEP = 20000;
  let fileLines = $state([]);
  let fileBase = $state(0);
  let fileEnd = 0;
  let fileMissing = $state(false);
  let fileLoading = $state(false);
  /** The file's start isn't shown (longer than one read, or than FILE_KEEP). */
  let fileCut = $state(false);
  /** Why the last read failed: shown in the view, not a toast every 5 s. */
  let fileErr = $state('');
  let fileTimer = null;
  let reading = false;
  /** Bumped when the view leaves the file: a read still out is dropped. */
  let fileGen = 0;

  async function readFile() {
    if (reading) return;
    reading = true;
    const gen = fileGen;
    try {
      const r = await api(`/servers/${encodeURIComponent(id)}/console/log?from=${fileEnd}`);
      if (gen !== fileGen || source !== 'file') return;
      fileMissing = r.missing;
      fileErr = '';
      const add = r.text ? r.text.replace(/\r?\n$/, '').split(/\r?\n/) : [];
      if (r.reset) {
        // A new run (or the file's start left out): numbers start over.
        fileBase = 0;
        fileLines = add;
        fileCut = !!r.cut;
      } else if (add.length) {
        fileLines = fileLines.concat(add);
      }
      if (fileLines.length > FILE_KEEP) {
        const drop = fileLines.length - FILE_KEEP;
        fileLines = fileLines.slice(drop);
        fileBase += drop;
        fileCut = true;
      }
      fileEnd = r.end;
    } catch (e) {
      if (gen === fileGen) fileErr = e.message;
    } finally {
      if (gen === fileGen) {
        reading = false;
        fileLoading = false;
      }
    }
  }

  /** Stops reading the file: the timer, and any read still out. */
  function stopFile() {
    clearInterval(fileTimer);
    fileTimer = null;
    fileGen++;
    reading = false;
  }

  function setSource(s) {
    if (source === s) return;
    source = s;
    stopFile();
    window.getSelection()?.removeAllRanges();
    picked = false;
    stuck = true;
    if (s === 'file') {
      fileLines = [];
      fileBase = 0;
      fileEnd = 0;
      fileCut = false;
      fileErr = '';
      fileMissing = false;
      fileLoading = true;
      readFile();
      fileTimer = setInterval(readFile, 5000);
    }
  }
  onDestroy(stopFile);

  const lines = $derived(source === 'file' ? fileLines : liveLines);
  const keyBase = $derived(source === 'file' ? fileBase : liveKeyBase);
  const kinds = $derived(lines.map(kind));

  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const out = [];
    for (let i = 0; i < lines.length; i++) {
      const k = kinds[i];
      const ok =
        filter === 'all' ||
        (filter === 'e' && (k === 'e' || k === 'f')) ||
        (filter === 'w' && (k === 'w' || k === 'e' || k === 'f')) ||
        (filter === 'j' && k === 'j') ||
        (filter === 'c' && k === 'c');
      if (ok && (!q || lines[i].toLowerCase().includes(q))) out.push({ text: lines[i], key: keyBase + i, k });
    }
    return out;
  });
  const counts = $derived({ e: kinds.filter((k) => k === 'e').length, w: kinds.filter((k) => k === 'w').length });
  const filtered = $derived(filter !== 'all' || !!query.trim());

  // Follows new lines while at the bottom and nothing is picked. The last
  // key too: once the live window is full, a new line keeps the count.
  $effect(() => {
    shown.length;
    shown[shown.length - 1]?.key;
    if (stuck && !picking && !picked) tick().then(() => box && (box.scrollTop = box.scrollHeight));
  });

  function onScroll() {
    stuck = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
  }

  function toBottom() {
    stuck = true;
    if (box) box.scrollTop = box.scrollHeight;
  }

  /** Whether the page's selection is inside the lines. */
  function selInLines() {
    const s = window.getSelection();
    if (!s || s.isCollapsed || !s.rangeCount || !inner) return false;
    return inner.contains(s.getRangeAt(0).commonAncestorContainer) || s.containsNode(inner, true);
  }

  function onSelChange() {
    picked = selInLines();
  }

  function down(e) {
    if (e.button === 0) picking = true;
  }

  function up() {
    if (!picking) return;
    picking = false;
    picked = selInLines();
  }

  function selectAll() {
    if (!inner) return;
    const r = document.createRange();
    r.selectNodeContents(inner);
    const s = window.getSelection();
    s.removeAllRanges();
    s.addRange(r);
    picked = true;
    menu = null;
  }

  async function copy() {
    menu = null;
    const text = window.getSelection()?.toString() || '';
    if (!text) return;
    // execCommand copies the picked text as it is, also over plain http.
    let ok = false;
    try {
      ok = document.execCommand('copy');
    } catch {
      ok = false;
    }
    if (!ok) {
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        say("Couldn't copy; press Ctrl+C instead.", true);
      }
    }
  }

  function keys(e) {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && e.key.toLowerCase() === 'a') {
      e.preventDefault();
      selectAll();
    } else if (e.key === 'Escape') {
      menu = null;
      if (picked) {
        window.getSelection()?.removeAllRanges();
        picked = false;
      }
    }
  }

  function context(e) {
    e.preventDefault();
    picked = selInLines();
    menu = { x: Math.min(e.clientX, innerWidth - 200), y: Math.min(e.clientY, innerHeight - 90) };
  }

  async function send(e) {
    e.preventDefault();
    const line = cmd.trim().replace(/^\//, '');
    if (!line || !alive) return;
    try {
      await post(`/servers/${encodeURIComponent(id)}/command`, { line });
      hist = [line, ...hist.filter((h) => h !== line)].slice(0, 50);
      histAt = -1;
      cmd = '';
      window.getSelection()?.removeAllRanges();
      picked = false;
      toBottom();
    } catch (err) {
      say(err.message, true);
    }
  }

  function cmdKeys(e) {
    if (e.key === 'ArrowUp' && hist.length) {
      histAt = Math.min(histAt + 1, hist.length - 1);
      cmd = hist[histAt];
      e.preventDefault();
    } else if (e.key === 'ArrowDown') {
      histAt = Math.max(histAt - 1, -1);
      cmd = histAt < 0 ? '' : hist[histAt];
      e.preventDefault();
    }
  }
</script>

<svelte:document onselectionchange={onSelChange} />
<svelte:window onpointerup={up} onblur={up} onclick={() => (menu = null)} onkeydown={(e) => e.key === 'Escape' && (menu = null)} />

<section class="term">
  <div class="tb">
    <div class="utabs" role="tablist" aria-label="Console source">
      <button role="tab" aria-selected={source === 'live'} class:tab-sel={source === 'live'} onclick={() => setSource('live')}>Live</button>
      <button role="tab" aria-selected={source === 'file'} class:tab-sel={source === 'file'} onclick={() => setSource('file')}>Whole log file</button>
    </div>
    <span class="tiny muted src">{source === 'file' ? "This run's latest.log from the start, read every few seconds." : "The panel's latest lines, live."}</span>
    <span class="sp"></span>
    <button class="btn sm" title="Upload a log or crash report and get a link for Discord or a bug report. The log becomes public." onclick={() => onShare('')}><Share2 size={13} />Share on mclo.gs</button>
  </div>
  <div class="tb">
    <button class="chip" class:on={filter === 'all'} onclick={() => (filter = 'all')}>All</button>
    <button class="chip" class:on={filter === 'e'} title="Errors and their stack traces" onclick={() => (filter = 'e')}>Errors {counts.e}</button>
    <button class="chip" class:on={filter === 'w'} title="Errors and warnings only" onclick={() => (filter = 'w')}>Warnings {counts.w}</button>
    <button class="chip" class:on={filter === 'j'} onclick={() => (filter = 'j')}>Joins</button>
    <button class="chip" class:on={filter === 'c'} onclick={() => (filter = 'c')}>Chat</button>
    {#if filtered}<span class="tiny muted">{shown.length} of {lines.length} lines</span>{/if}
    <span class="sp"></span>
    <div class="input find"><Search size={13} /><input use:rightClear placeholder="Search this log" bind:value={query} spellcheck="false" />{#if query}<button class="x" title="Clear" onclick={() => (query = '')}><X size={12} /></button>{/if}</div>
  </div>
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div class="lines" bind:this={box} onscroll={onScroll} onpointerdown={down} oncontextmenu={context} onkeydown={keys} tabindex="0" role="log" aria-label="Console">
    {#if source === 'file' && fileErr}<div class="err note">Couldn't read latest.log: {fileErr}</div>{/if}
    {#if source === 'file' && fileLoading}
      <div class="muted note row"><span class="spinner"></span>Reading latest.log…</div>
    {:else if !shown.length}
      {#if lines.length}
        <div class="muted note">Nothing matches.</div>
      {:else}
        <div class="empty-state"><div class="es-ic"><SquareTerminal size={18} /></div>
          <b>{source === 'file' && fileMissing ? 'No latest.log yet' : alive ? 'No console output yet.' : 'The server is stopped'}</b>
          <p>{source === 'file' && fileMissing ? 'It shows once the server has run.' : alive ? 'Lines show here as the server writes them.' : 'Its console shows here when it runs.'}</p></div>
      {/if}
    {:else if source === 'file' && fileCut}
      <div class="muted note tiny">The start of the file isn't shown here (it's too long). Share on mclo.gs or open logs/latest.log in Files for all of it.</div>
    {/if}
    <div class="inner" bind:this={inner} style="--gw: {Math.max(3, String(keyBase + lines.length).length) + 2}ch">
      {#each shown as l (l.key)}
        <div class="ln {l.k}" data-n={l.key + 1}>{l.text}</div>
      {/each}
    </div>
  </div>
  {#if !stuck}
    <button class="btn sm jump" onclick={toBottom}><ArrowDown size={13} />Latest</button>
  {/if}
  <form class="cmd" onsubmit={send}>
    <span class="mono slash">/</span>
    <input bind:value={cmd} onkeydown={cmdKeys} placeholder="Console command, e.g. say Restarting soon" disabled={!alive} spellcheck="false" autocomplete="off" aria-label="Console command" />
    <button class="btn sm pri" disabled={!alive || !cmd.trim()}><CornerDownLeft size={13} />Send</button>
  </form>
  {#if !alive}<div class="tiny muted off">Start the server to send commands.</div>{/if}
</section>

{#if menu}
  <div class="popover menu ctx" style="left: {menu.x}px; top: {menu.y}px" role="menu" tabindex="-1" onpointerdown={(e) => e.preventDefault()}>
    {#if picked}<button class="menu-item" role="menuitem" onclick={copy}><Copy size={13} />Copy</button>{/if}
    <button class="menu-item" role="menuitem" onclick={selectAll}><TextSelect size={13} />Select all</button>
  </div>
{/if}

<style>
  .term { flex: 1; min-width: 0; min-height: 360px; background: var(--term); border: 1px solid var(--line); border-radius: var(--radius-lg); display: flex; flex-direction: column; overflow: hidden; position: relative; }
  .tb { display: flex; align-items: center; gap: 6px; padding: 7px 10px; border-bottom: 1px solid var(--line); background: var(--base); flex-wrap: wrap; }
  .src { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .find { padding: 3px 8px; width: 200px; }
  .find .x { color: var(--muted); display: grid; place-items: center; }
  .lines { flex: 1; overflow: auto; padding: 10px 12px; font: 12px/1.6 var(--mono); color: var(--text); cursor: text; outline: none; }
  .lines:focus-visible { box-shadow: inset 0 0 0 1px var(--acc); }
  .note { padding-left: 2px; margin-bottom: 6px; }
  .lines .empty-state { height: 100%; cursor: default; }
  .ln { position: relative; padding-left: var(--gw); white-space: pre-wrap; word-break: break-word; content-visibility: auto; contain-intrinsic-size: auto 19px; }
  /* The number is drawn, not text: picking and copying leave it out. */
  .ln::before { content: attr(data-n); position: absolute; left: 0; width: calc(var(--gw) - 2ch); text-align: right; color: var(--muted); opacity: .6; user-select: none; }
  .ln::selection { background: #f9731659; }
  .ln.e { color: var(--red); } .ln.w { color: var(--gold); } .ln.j { color: var(--green); }
  .ln.h { color: var(--acc); } .ln.f { color: var(--subtle); }
  .jump { position: absolute; right: 16px; bottom: 60px; box-shadow: 0 6px 20px #0008; }
  .cmd { display: flex; align-items: center; gap: 8px; border-top: 1px solid var(--line); padding: 8px 10px; background: var(--base); }
  .cmd input { flex: 1; min-width: 0; background: none; border: 0; outline: 0; font: 12.5px var(--mono); color: var(--text); }
  .slash { color: var(--muted); }
  .off { padding: 0 10px 8px; background: var(--base); }
  .ctx { position: fixed; z-index: 80; }
</style>
