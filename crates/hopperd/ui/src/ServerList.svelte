<script>
  // The server list down the leading side, like Hopper desktop's Servers
  // page: a state light, the name, a pill for running / starting / failing
  // servers (a stopped one is just its grey dot), and a quiet caption.
  // Running servers first. Drag the edge to resize; dragged narrow, it
  // becomes a rail of tiles (desktop's server_rail). Right-click a server
  // for desktop's row menu: Open, Start / Stop / Restart, Open folder.
  //
  // Other panels added to this page each get a group under this panel's,
  // like desktop's server groups: chevron, light, name, "Hopper Server · N",
  // its health (version and round trip) or what's wrong, in place. Each
  // group loads on its own (lib/panels.svelte.js), so a dead one never
  // holds the list.
  //
  // Select (the tick-list button) picks servers across every group: click
  // or drag across rows, Shift+click for a range, Ctrl+A for all shown.
  // The bar under the list then starts, stops, restarts or sends a command
  // to them, after one question that lists what happens on each panel
  // (BulkAction.svelte). Filters above the rows narrow to one panel or one
  // state.
  import { tick } from 'svelte';
  import { Plus, RefreshCw, PanelLeftClose, PanelLeftOpen, Server, Play, Square, RotateCw, FolderOpen, Copy, Compass, ChevronRight, ChevronDown, Ellipsis, ExternalLink, LogOut, Trash2, ListChecks, Terminal, Pause } from '@lucide/svelte';
  import { put, post } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { panels, retry, signOut, removePanel } from './lib/panels.svelte.js';
  import { tokenFor } from './lib/panelTokens.js';
  import { roleLabel } from './lib/me.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import PanelSignIn from './PanelSignIn.svelte';
  import AddPanel from './AddPanel.svelte';
  import { power } from './lib/power.js';
  import { say } from './lib/toast.svelte.js';
  import { live } from './lib/live.svelte.js';
  import { packUpdates } from './lib/packs.svelte.js';
  import { stateText, stateKey, stateTip, bootText, loaderText } from './lib/format.js';
  import Avatar from './Avatar.svelte';
  import Progress from './Progress.svelte';
  import { making, cancel, forget } from './lib/making.svelte.js';
  import { startPick, clickSwallowed, tickLabel } from './lib/dragpick.svelte.js';
  import { Selection } from './lib/selection.svelte.js';
  import { passes } from './lib/bulk.js';
  import SelectionBar from './lib/SelectionBar.svelte';
  import BulkAction from './BulkAction.svelte';

  let { selected = null, selectedPanel = null, owner = false, canPower = true, homeName = '', homeVersion = '', homeRole = '', onNew, onRescan } = $props();

  // ---- groups: this panel, then each added one ----
  const FOLD = 'hopperd.servers.fold';
  let fold = $state((() => {
    try {
      return JSON.parse(localStorage.getItem(FOLD) || '{}');
    } catch {
      return {};
    }
  })());
  function flip(id) {
    fold[id] = !fold[id];
    try {
      localStorage.setItem(FOLD, JSON.stringify(fold));
    } catch {
      // private window: not kept
    }
  }
  // This panel's nodes: a group each, right under this panel's own servers.
  const nodes = $derived(live.nodes || []);
  const grouped = $derived(panels.list.length > 0 || nodes.length > 0);
  const nodeKey = (n) => `node:${n.id}`;
  /** A node's light: green connected, amber reconnecting, new, updating, under maintenance or needing an update, red offline. */
  const nodeLight = (n) => (n.state === 'offline' ? 'crashed' : n.state === 'connected' && !n.maintenance && n.update?.state !== 'updating' ? 'running' : 'starting');
  // Updating a node runs its own update (design/multi-panel.md, A3).
  async function updateNode(n) {
    if (!(await confirmIt(`Update ${n.name} to hopperd ${homeVersion}?`, { body: 'It downloads the new Hopper Server from GitHub and restarts into it, as its own update does. Its servers keep running.', ok: 'Update' }))) return;
    try {
      await post(`/nodes/${encodeURIComponent(n.id)}/update`, {}, { home: true });
      say(`Updating ${n.name}`);
    } catch (e) {
      say(e.message, true);
    }
  }
  const CEILING = { look: 'Look only', operate: 'Operate', manage: 'Manage' };
  const secsAgo = (t) => (t ? ago(t * 1000) : '');
  /** The node's health line: what it runs, its machine, and the RAM its servers may take. */
  function nodeHealth(n) {
    const h = n.host;
    const parts = ['Node'];
    if (h) {
      parts.push(`CPU ${Math.round(h.cpu)}%`);
      if (h.mem_total) parts.push(`RAM ${Math.round(h.mem_used / 2 ** 30)} of ${Math.round(h.mem_total / 2 ** 30)} GB`);
      if (h.disk_total) parts.push(`${bytes(h.disk_free)} free`);
    }
    // RAM promised: what its servers may take at most, against what it has.
    if (n.promised) parts.push(`${bytes(n.promised)} promised${h?.mem_total && n.promised > h.mem_total ? ', more than it has' : ''}`);
    if (n.rtt_ms != null) parts.push(`${n.rtt_ms} ms`);
    return parts.join(' · ');
  }
  async function maintain(n, on) {
    if (on && !(await confirmIt(`Put ${n.name} under maintenance?`, { body: 'Its alerts go quiet and nothing new starts there from this panel. Servers that are running keep running.', ok: 'Pause node' }))) return;
    try {
      await put(`/nodes/${encodeURIComponent(n.id)}/maintenance`, { on }, { home: true });
      say(on ? `${n.name} is under maintenance` : `${n.name} is out of maintenance`);
    } catch (e) {
      say(e.message, true);
    }
  }
  // Only people signed in with a password keep a list of panels.
  const canAdd = $derived(homeRole && homeRole !== 'device' && homeRole !== 'token');
  let adding = $state(false);
  const sorted = (list) => [...list].sort((a, b) => Number(alive(b)) - Number(alive(a)));
  /** The group's light: green up, amber on its way or waiting for a sign-in, red when it can't be used. */
  const light = (p) => (p.state === 'ok' ? 'running' : p.state === 'loading' || p.state === 'reconnecting' || p.state === 'signin' ? 'starting' : 'crashed');
  const dim = (p) => p.state !== 'ok';
  const canPowerOn = (p) => (p ? p.role !== 'viewer' : canPower);
  function ago(ms) {
    const s = Math.round((Date.now() - ms) / 1000);
    if (s < 60) return 'just now';
    if (s < 3600) return `${Math.round(s / 60)} min ago`;
    return `${Math.round(s / 3600)} h ago`;
  }
  const serverHref = (p, s, rest = '') => `#${p ? `/p/${encodeURIComponent(p.id)}` : ''}/server/${encodeURIComponent(s.id)}${rest}`;
  const isSel = (p, s) => selected === s.id && (p ? selectedPanel === p.id : !selectedPanel);

  const KEY = 'hopperd.servers.list';
  const MIN = 200, MAX = 420, DEF = 248, RAIL = 64;
  const read = () => {
    try {
      return JSON.parse(localStorage.getItem(KEY) || '{}');
    } catch {
      return {};
    }
  };
  const saved = read();
  let width = $state(Math.min(MAX, Math.max(MIN, Number.isFinite(saved.w) ? saved.w : DEF)));
  let collapsed = $state(!!saved.c);
  let drag = $state(null);

  function save() {
    try {
      localStorage.setItem(KEY, JSON.stringify({ w: width, c: collapsed }));
    } catch {
      // private window: not kept
    }
  }

  const alive = (s) => s.state === 'running' || s.state === 'starting' || s.state === 'stopping';
  const servers = $derived([...live.servers].sort((a, b) => Number(alive(b)) - Number(alive(a))));
  // A server whose Ready row is up shows once, as that row.
  const shown = $derived(servers.filter((s) => !making.some((m) => m.state === 'ready' && m.id === s.id)));
  const openMade = (m) => { forget(m.job); if (m.id) location.hash = `/server/${encodeURIComponent(m.id)}/console`; };
  const startMade = (m) => { forget(m.job); if (m.id) { power({ id: m.id, name: m.name, state: 'stopped' }, 'start'); location.hash = `/server/${encodeURIComponent(m.id)}/console`; } };
  const loading = $derived(!live.connected && !live.servers.length);

  // ---- filters: one panel, one state (kept per browser) ----
  const FILTER = 'hopperd.servers.filter';
  let filter = $state((() => {
    try {
      const f = JSON.parse(localStorage.getItem(FILTER) || '{}');
      return { panel: typeof f.panel === 'string' ? f.panel : 'all', state: typeof f.state === 'string' ? f.state : 'all' };
    } catch {
      return { panel: 'all', state: 'all' };
    }
  })());
  function setFilter(k, v) {
    filter[k] = v;
    try {
      localStorage.setItem(FILTER, JSON.stringify(filter));
    } catch {
      // private window: not kept
    }
  }
  const STATES = [['all', 'All'], ['running', 'Running'], ['stopped', 'Stopped'], ['crashed', 'Crashed']];
  // A saved filter for a panel that's no longer on the page shows them all.
  const panelFilter = $derived(!grouped ? 'all' : filter.panel === 'home' || panels.list.some((p) => p.id === filter.panel) || nodes.some((n) => nodeKey(n) === filter.panel) ? filter.panel : 'all');
  const showGroup = (id) => panelFilter === 'all' || panelFilter === id;
  const homeAll = $derived(shown.filter((s) => !s.node));
  const homeRows = $derived(homeAll.filter((s) => passes(s, filter.state)));
  const nodeRows = (n) => shown.filter((s) => s.node === n.id && passes(s, filter.state));
  const nodeOf = (s) => (s.node ? nodes.find((n) => n.id === s.node) : null);
  const rowsOf = (p) => sorted(p.servers).filter((s) => passes(s, filter.state));
  const noneText = $derived(`No ${filter.state} servers here.`);
  const anyServers = $derived(live.servers.length > 0 || panels.list.some((p) => p.servers.length));

  // ---- picking servers for a bulk action ----
  let selecting = $state(false);
  let picked = $state(new Set());
  const picks = new Selection({ get: () => picked, set: (v) => (picked = v) });
  const keyOf = (p, s) => `${p ? p.id : ''}|${s.id}`;
  /** Every row the list shows, in order: for Shift ranges and drags. */
  const flat = $derived.by(() => {
    const out = [];
    if (showGroup('home') && (!grouped || !fold.home)) for (const s of homeRows) out.push({ key: keyOf(null, s), p: null, s });
    for (const n of nodes) {
      if (!showGroup(nodeKey(n)) || fold[nodeKey(n)]) continue;
      for (const s of nodeRows(n)) out.push({ key: keyOf(null, s), p: null, s });
    }
    for (const p of panels.list) {
      if (!showGroup(p.id) || p.state === 'signin' || fold[p.id]) continue;
      for (const s of rowsOf(p)) out.push({ key: keyOf(p, s), p, s });
    }
    return out;
  });
  const order = $derived(flat.map((r) => r.key));
  const ixOf = $derived(new Map(order.map((k, i) => [k, i])));
  // Only what is still shown counts: a filter or a fold lets go of the rest.
  const pickedRows = $derived(flat.filter((r) => picked.has(r.key)));

  function stopSelecting() {
    selecting = false;
    picked = new Set();
  }

  /** A row's click while selecting: tick it (Shift: the rows from the last one). */
  function rowClick(e, p, s) {
    if (!selecting) return;
    e.preventDefault();
    if (clickSwallowed()) return;
    picks.tick(keyOf(p, s), e, order);
  }

  /** Pressing a row while selecting and dragging ticks every row it crosses.
   * Not on touch: there a drag scrolls, and a tap ticks. */
  function rowDown(e, p, s) {
    if (!selecting || e.shiftKey || e.pointerType === 'touch') return;
    const key = keyOf(p, s);
    const on = !picked.has(key);
    startPick(e, { list: 'servers', kind: 'tick', ix: ixOf.get(key), on, selected: picked, keyAt: (i) => order[i] ?? null, onTick: (v) => (picked = v), label: tickLabel(on) });
  }

  function listKey(e) {
    if (!selecting) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      stopSelecting();
      return;
    }
    picks.onKey(e, order, { arrows: false });
  }

  let bulk = $state(null); // { action, groups } while its question is open

  /** Opens the one question for `action` on the picked servers, grouped by panel. */
  function openBulk(action) {
    const groups = [
      { key: 'home', name: homeName || 'This panel', panel: null, role: canPower ? homeRole || 'owner' : 'viewer', state: 'ok', servers: [] },
      ...panels.list.map((p) => ({ key: p.id, name: p.name, panel: p, role: p.role, state: p.state, servers: [] })),
    ];
    const at = new Map(groups.map((g) => [g.key, g]));
    for (const r of pickedRows) at.get(r.p ? r.p.id : 'home')?.servers.push($state.snapshot(r.s));
    bulk = { action, groups };
  }

  /** The pill: players while it runs, else its state; none when stopped. */
  function pill(s) {
    if (s.state === 'running') return { text: `${s.players.length}/${s.max_players}`, glow: true };
    if (s.state === 'starting' || s.state === 'stopping' || s.state === 'crashed' || s.wake_failed) return { text: stateText(s), glow: false };
    return null;
  }

  function caption(s) {
    const parts = [];
    const lv = [loaderText(s.loader, true), s.version].filter(Boolean).join(' ');
    if (lv) parts.push(lv);
    if (s.addresses?.length) parts.push(s.addresses[0]);
    if (s.managed_by) parts.push(`also in ${s.managed_by}`);
    if (s.sleep_after_minutes && s.state === 'stopped') parts.push('sleeping');
    return parts.join(' · ');
  }

  const initials = (n) => (n.split(/[^\p{L}\p{N}]+/u).filter(Boolean).map((w) => w[0]).slice(0, 2).join('') || n[0] || '?').toUpperCase();

  function down(e) {
    e.preventDefault();
    drag = { x: e.clientX, w: collapsed ? RAIL : width };
    document.body.classList.add('resizing');
    const move = (ev) => {
      const w = drag.w + ev.clientX - drag.x;
      if (w < 150) collapsed = true;
      else {
        collapsed = false;
        width = Math.min(MAX, Math.max(MIN, w));
      }
    };
    const up = () => {
      drag = null;
      document.body.classList.remove('resizing');
      removeEventListener('pointermove', move);
      removeEventListener('pointerup', up);
      removeEventListener('pointercancel', up);
      save();
    };
    addEventListener('pointermove', move);
    addEventListener('pointerup', up);
    addEventListener('pointercancel', up);
  }

  /** The grip from the keyboard: arrows resize, Enter shows or hides the list. */
  function gripKey(e) {
    const d = e.key === 'ArrowLeft' ? -16 : e.key === 'ArrowRight' ? 16 : 0;
    if (d) {
      e.preventDefault();
      if (collapsed && d > 0) collapsed = false;
      else if (!collapsed && d < 0 && width <= MIN) collapsed = true;
      else if (!collapsed) width = Math.min(MAX, Math.max(MIN, width + d));
      save();
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      toggle();
    }
  }

  // ---- the row menu ----
  let menu = $state(null); // { s, p, x, y } for a server, { g, x, y } for a group
  let menuEl = $state();

  async function openMenu(e, s, p = null, g = null) {
    e.preventDefault();
    // From the keyboard (the menu key, Shift+F10) there's no pointer: under the row.
    let { clientX: x, clientY: y } = e;
    if (!x && !y) {
      const r = e.currentTarget.getBoundingClientRect();
      x = r.left + 12;
      y = r.bottom;
    }
    menu = { s, p, g, x, y };
    await tick();
    if (!menuEl) return;
    const r = menuEl.getBoundingClientRect();
    menu.x = Math.max(8, Math.min(x, innerWidth - r.width - 8));
    menu.y = Math.max(8, y + r.height > innerHeight - 8 ? y - r.height : y);
    menuEl.querySelector('button')?.focus();
  }

  function closeMenu() {
    menu = null;
  }

  function pick(f) {
    const m = menu;
    closeMenu();
    if (m) f(m.s ?? m.g, m.p);
  }

  async function drop(p) {
    if (!(await confirmIt(`Remove ${p.name} from this page?`, { body: 'Its servers leave this list and this browser signs out of it. Nothing changes on that panel.', ok: 'Remove' }))) return;
    removePanel(p);
  }

  function menuKeys(e) {
    if (e.key === 'Escape' || e.key === 'Tab') {
      e.preventDefault();
      closeMenu();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const items = [...menuEl.querySelectorAll('button')];
      const i = items.indexOf(document.activeElement);
      const n = e.key === 'ArrowDown' ? (i + 1) % items.length : (i - 1 + items.length) % items.length;
      items[n]?.focus();
    }
  }

  function away(e) {
    if (menu && menuEl && !menuEl.contains(e.target)) closeMenu();
  }

  async function copyAddress(s) {
    try {
      await navigator.clipboard.writeText(s.addresses[0]);
      say(`Copied ${s.addresses[0]}`);
    } catch {
      say("Can't copy here.", true);
    }
  }

  function toggle() {
    collapsed = !collapsed;
    save();
  }

</script>

{#snippet row(s, p)}
  {@const pl = pill(s)}
  {@const cap = caption(s)}
  {@const up = p ? null : packUpdates.byId[s.id]}
  {@const key = keyOf(p, s)}
  {@const ticked = selecting && picked.has(key)}
  {@const sel = selecting ? ticked : isSel(p, s)}
  {@const off = (p && dim(p)) || s.stale}
  <a class="srv" class:picking={selecting} class:dim={off} class:row-sel={sel} class:row-hover={!sel} href={serverHref(p, s)} data-pick-ix={ixOf.get(key)}
    role={selecting ? 'checkbox' : undefined} aria-checked={selecting ? ticked : undefined} draggable={selecting ? 'false' : undefined}
    onclick={(e) => rowClick(e, p, s)} onpointerdown={(e) => rowDown(e, p, s)} ondragstart={(e) => selecting && e.preventDefault()} oncontextmenu={(e) => openMenu(e, s, p)}>
    <div class="line">
      {#if selecting}<input class="ck" type="checkbox" tabindex="-1" aria-hidden="true" checked={ticked} />{/if}
      <span class="dot {off ? '' : stateKey(s)}"></span>
      <Avatar server={s} cls="sic" panel={p} />
      <span class="nm">{s.name}</span>
      {#if up}<span class="upd" title="Pack update: {up.from} → {up.to}"></span>{/if}
      {#if pl && !off}<span class="pill {stateKey(s)}" title={bootText(s) || stateTip(s)}>{#if pl.glow}<span class="glow"></span>{/if}{pl.text}</span>{/if}
    </div>
    {#if p && dim(p)}<div class="cap">{p.lastSeen ? `Seen ${ago(p.lastSeen)}` : 'Not seen yet'}</div>
    {:else if s.stale}<div class="cap">Node offline · last known state</div>
    {:else if s.progress}<Progress server={s} compact />{:else if cap}<div class="cap">{cap}</div>{/if}
  </a>
{/snippet}

{#snippet head(id, name, chipText, lightKey, p)}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div class="gh" role="group" aria-label={name} oncontextmenu={p ? (e) => openMenu(e, null, null, p) : undefined}>
    <button class="gfold" aria-expanded={!fold[id]} title="{name}: {fold[id] ? 'show its servers' : 'hide its servers'}" onclick={() => flip(id)}>
      {#if fold[id]}<ChevronRight size={14} />{:else}<ChevronDown size={14} />{/if}
      <span class="dot {lightKey}"></span>
      <span class="gnm">{name}</span>
    </button>
    <span class="gfill"></span>
    {#if chipText}<span class="chip">{chipText}</span>{/if}
    {#if p}<button class="btn ghost icon sm gmore" title="More" aria-label="More for {name}" onclick={(e) => openMenu(e, null, null, p)}><Ellipsis size={14} /></button>{/if}
  </div>
{/snippet}

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<aside class="list" class:rail={collapsed} style:width="{collapsed ? RAIL : width}px" data-pick-list="servers" onkeydown={listKey}>
  {#if collapsed}
    <button class="btn ghost icon" title="Show server list" onclick={toggle}><PanelLeftOpen size={15} /></button>
    {#each servers as s (s.id)}
      <a class="tile" class:row-sel={isSel(null, s)} class:row-hover={!isSel(null, s)} href={serverHref(null, s)} title={s.name} oncontextmenu={(e) => openMenu(e, s)}>
        {#if s.icon}<Avatar server={s} cls="sic tic" panel={null} />{:else}{initials(s.name)}{/if}
        <span class="dot {stateKey(s)} tdot"></span>
      </a>
    {/each}
    {#each panels.list as p (p.id)}
      {#if p.servers.length}<div class="rsep" title={p.name}></div>{/if}
      {#each sorted(p.servers) as s (s.id)}
        <a class="tile" class:dim={dim(p)} class:row-sel={isSel(p, s)} class:row-hover={!isSel(p, s)} href={serverHref(p, s)} title="{s.name} · {p.name}" oncontextmenu={(e) => openMenu(e, s, p)}>
          {#if s.icon}<Avatar server={s} cls="sic tic" panel={p} />{:else}{initials(s.name)}{/if}
          <span class="dot {dim(p) ? '' : stateKey(s)} tdot"></span>
        </a>
      {/each}
    {/each}
  {:else}
    <div class="hd">
      <span class="title">Servers</span>
      {#if owner}<button class="btn ghost icon" title="Look for new server folders" onclick={onRescan}><RefreshCw size={14} /></button>{/if}
      <a class="btn ghost icon" title="Browse modpacks to make a server from" href="#/browse"><Compass size={15} /></a>
      <button class="btn ghost icon" title="New server" onclick={onNew}><Plus size={15} /></button>
      {#if anyServers}<button class="btn ghost icon" class:on={selecting} aria-pressed={selecting} title={selecting ? 'Stop selecting (Esc)' : 'Select servers to start, stop or restart together'} onclick={() => (selecting ? stopSelecting() : (selecting = true))}><ListChecks size={15} /></button>{/if}
      <button class="btn ghost icon" title="Hide server list" onclick={toggle}><PanelLeftClose size={15} /></button>
    </div>
    {#if anyServers}
      <div class="fbar">
        {#if grouped}
          <select class="sel" aria-label="Show the servers of" value={panelFilter} onchange={(e) => setFilter('panel', e.currentTarget.value)}>
            <option value="all">All panels</option>
            <option value="home">{homeName || 'This panel'}</option>
            {#each nodes as n (n.id)}<option value={nodeKey(n)}>{n.name}</option>{/each}
            {#each panels.list as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
          </select>
        {/if}
        <div class="seg" role="group" aria-label="Show servers that are">
          {#each STATES as [k, l] (k)}<button aria-pressed={filter.state === k} onclick={() => setFilter('state', k)}>{l}</button>{/each}
        </div>
      </div>
      {#if selecting && !pickedRows.length}<div class="hint">Click or drag across servers to pick them.</div>{/if}
    {/if}
    {#if grouped && showGroup('home')}
      {@render head('home', homeName || 'This panel', homeRole ? roleLabel(homeRole) : '', live.connected ? 'running' : 'starting', null)}
      <div class="health">Hopper Server · {homeAll.length} · this panel{homeVersion ? ` · ${homeVersion}` : ''}</div>
    {/if}
    {#if showGroup('home') && (!grouped || !fold.home)}
      {#if loading}
        <div class="skels">{#each [0, 1, 2] as i (i)}<div class="skel"><i></i></div>{/each}</div>
      {/if}
      {#each making as m (m.job)}
        <div class="srv making" class:ready={m.state === 'ready'}>
          <div class="line">
            <span class="dot {m.state === 'failed' ? 'crashed' : m.state === 'ready' ? 'running' : 'starting'}"></span>
            <Avatar server={{ name: m.name }} cls="sic" panel={null} />
            <span class="nm">{m.name}</span>
          </div>
          <div class="cap mk" class:bad={m.state === 'failed'} class:ok={m.state === 'ready'} title={m.error || m.detail}>
            {m.state === 'running' ? m.detail || 'Getting ready' : m.state === 'cancelled' ? 'Cancelled. Nothing was made.' : m.state === 'failed' ? m.error.split('\n')[0] : `${m.detail || 'Ready'}${m.start ? ' · starting' : ''}`}
          </div>
          {#if m.state === 'running'}<div class="mkbar"><i style:width="{Math.max(3, (m.progress || 0) * 100)}%"></i></div>{/if}
          <div class="mkact">
            {#if m.state === 'running'}<button class="btn sm" onclick={() => cancel(m.job)}>Cancel</button>
            {:else if m.state === 'ready'}
              {#if !m.start && canPower}<button class="btn sm go" onclick={() => startMade(m)}><Play size={12} />Start</button>{/if}
              <button class="btn sm" onclick={() => openMade(m)}>Open</button>
            {:else}<button class="btn sm ghost" onclick={() => forget(m.job)}>Dismiss</button>{/if}
          </div>
        </div>
      {/each}
      {#each homeRows as s (s.id)}
        {@render row(s, null)}
      {:else}
        {#if homeAll.length}
          <div class="cap none" class:flat={!grouped}>{noneText}</div>
        {:else if shown.length && !making.length}
          <div class="cap none">No servers on this panel itself.</div>
        {:else if !loading && !making.length}
          <div class="empty-state">
            <span class="es-ic"><Server size={18} /></span>
            <b>No servers yet</b>
            <p>Make one with New server{owner ? ', or put server folders in the panel folder' : ''}.</p>
            <button class="btn sm pri" onclick={onNew}><Plus size={13} />New server</button>
            <a class="btn sm" href="#/browse"><Compass size={13} />Browse modpacks</a>
          </div>
        {/if}
      {/each}
    {/if}

    {#each nodes.filter((n) => showGroup(nodeKey(n))) as n, i (n.id)}
      {@const k = nodeKey(n)}
      {#if showGroup('home') || i > 0}<div class="gsep"></div>{/if}
      {@render head(k, n.name, CEILING[n.ceiling] || '', nodeLight(n), owner ? { node: n } : null)}
      {#if n.maintenance}
        <div class="health maint"><Pause size={11} /> Under maintenance{n.maintenance.by ? ` · by ${n.maintenance.by}` : ''}{n.maintenance.since ? ` ${secsAgo(n.maintenance.since)}` : ''} · nothing starts here{#if owner}{' · '}<button class="link" onclick={() => maintain(n, false)}>End maintenance</button>{/if}</div>
      {/if}
      {#if n.update?.state === 'updating'}
        <div class="health">Updating · {n.update.message}</div>
      {:else if n.state === 'update_needed'}
        <div class="health trouble" title={n.skew}>Update needed · update it on the node itself</div>
      {:else if n.state === 'main_older'}
        <div class="health trouble" title={n.skew}>Newer than this panel · update this panel first</div>
      {:else if n.state === 'connected' || n.state === 'reconnecting'}
        <div class="health">{nodeHealth(n)}{n.state === 'reconnecting' ? ' · Reconnecting…' : ''}{n.can_update ? ' · update available' : ''}</div>
      {:else if n.state === 'new'}
        <div class="health">Node · waiting for it to connect</div>
      {:else}
        <div class="health trouble">Offline{n.last_seen ? ` · last seen ${secsAgo(n.last_seen)}` : ''}</div>
      {/if}
      {#if !fold[k]}
        {#each nodeRows(n) as s (s.id)}
          {@render row(s, null)}
        {:else}
          {#if n.count}<div class="cap none">{noneText}</div>
          {:else if n.state === 'connected'}<div class="cap none">No servers on it yet.</div>{/if}
        {/each}
      {/if}
    {/each}

    {#each panels.list.filter((q) => showGroup(q.id)) as p, i (p.id)}
      {#if showGroup('home') || nodes.some((n) => showGroup(nodeKey(n))) || i > 0}<div class="gsep"></div>{/if}
      {@render head(p.id, p.name, p.me && p.state !== 'signin' ? roleLabel(p.role) : '', light(p), p)}
      {#if p.state === 'ok'}
        <div class="health">Hopper Server · {p.servers.length}{p.version ? ` · ${p.version}` : ''}{p.ping != null ? ` · ${p.ping} ms` : ''}</div>
      {:else if p.state === 'loading'}
        <div class="health">Connecting…</div>
      {:else if p.state === 'reconnecting' || p.state === 'offline'}
        <div class="health trouble">Can't reach {p.name}{p.state === 'reconnecting' ? ' · Reconnecting…' : ''} · <button class="link" onclick={() => retry(p)}>Try again</button></div>
      {:else if p.state === 'refused'}
        <div class="health trouble">{p.error || `${p.name} doesn't let this page in.`} On its page, add {location.origin} in Settings › Access & devices › Other web pages. <a class="link" href={p.url} target="_blank" rel="noopener">Open its page</a> · <button class="link" onclick={() => retry(p)}>Try again</button></div>
      {:else if p.state === 'old'}
        <div class="health trouble">Runs Hopper Server {p.version}, too old to use from here. <a class="link" href={p.url} target="_blank" rel="noopener">Update it on its page</a></div>
      {:else if p.state === 'setup'}
        <div class="health trouble">Not set up yet. <a class="link" href={p.url} target="_blank" rel="noopener">Open its page</a> to make its owner account.</div>
      {/if}
      {#if p.state === 'signin'}
        <PanelSignIn panel={p} />
      {:else if !fold[p.id]}
        {#each rowsOf(p) as s (s.id)}
          {@render row(s, p)}
        {:else}
          {#if p.servers.length}<div class="cap none">{noneText}</div>
          {:else if p.state === 'ok'}<div class="cap none">No servers on it yet.</div>{/if}
        {/each}
      {/if}
    {/each}

    {#if canAdd}
      <button class="btn sm ghost addp" onclick={() => (adding = true)}><Plus size={13} />Add a Hopper Server</button>
    {/if}
    {#if selecting && pickedRows.length}
      <SelectionBar n={pickedRows.length} onClear={stopSelecting}>
        <button class="btn sm ghost" onclick={() => openBulk('start')}><Play size={13} />Start</button>
        <button class="btn sm ghost" onclick={() => openBulk('stop')}><Square size={13} />Stop</button>
        <button class="btn sm ghost" onclick={() => openBulk('restart')}><RotateCw size={13} />Restart</button>
        <button class="btn sm ghost" onclick={() => openBulk('command')}><Terminal size={13} />Command</button>
      </SelectionBar>
    {/if}
  {/if}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div class="grip" class:on={drag} role="separator" aria-orientation="vertical" aria-label="Server list width" aria-valuenow={collapsed ? RAIL : width} aria-valuemin={RAIL} aria-valuemax={MAX} tabindex="0" title="Drag to resize" onpointerdown={down} ondblclick={toggle} onkeydown={gripKey}></div>
</aside>

<svelte:window onpointerdown={away} onresize={closeMenu} onblur={closeMenu} />

{#if adding}<AddPanel onClose={() => (adding = false)} />{/if}
{#if bulk}<BulkAction action={bulk.action} groups={bulk.groups} onClose={() => (bulk = null)} />{/if}

{#if menu?.g?.node}
  {@const n = menu.g.node}
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div class="popover menu rmenu" role="menu" tabindex="-1" bind:this={menuEl} style:left="{menu.x}px" style:top="{menu.y}px" onkeydown={menuKeys}>
    {#if n.maintenance}<button class="menu-item" role="menuitem" onclick={() => pick(() => maintain(n, false))}><Play size={14} />End maintenance</button>
    {:else}<button class="menu-item" role="menuitem" onclick={() => pick(() => maintain(n, true))}><Pause size={14} />Pause node (maintenance)</button>{/if}
    {#if n.can_update}<button class="menu-item" role="menuitem" onclick={() => pick(() => updateNode(n))}><RefreshCw size={14} />Update to hopperd {homeVersion}</button>{/if}
    <div class="menu-sep"></div>
    <button class="menu-item" role="menuitem" onclick={() => pick(() => (location.hash = '/settings/nodes'))}><Server size={14} />Node settings</button>
  </div>
{:else if menu?.g}
  {@const g = menu.g}
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div class="popover menu rmenu" role="menu" tabindex="-1" bind:this={menuEl} style:left="{menu.x}px" style:top="{menu.y}px" onkeydown={menuKeys}>
    <button class="menu-item" role="menuitem" onclick={() => pick((v) => window.open(v.url, '_blank', 'noopener'))}><ExternalLink size={14} />Open in browser</button>
    {#if g.state !== 'ok' && g.state !== 'signin'}<button class="menu-item" role="menuitem" onclick={() => pick((v) => retry(v))}><RotateCw size={14} />Try again</button>{/if}
    {#if tokenFor(g.url) && g.state !== 'signin'}<button class="menu-item" role="menuitem" onclick={() => pick((v) => signOut(v))}><LogOut size={14} />Sign out of it</button>{/if}
    <div class="menu-sep"></div>
    <button class="menu-item" role="menuitem" onclick={() => pick(drop)}><Trash2 size={14} />Remove from this page</button>
  </div>
{:else if menu}
  {@const s = menu.s}
  {@const up = s.state === 'running' || s.state === 'starting'}
  {@const nd = nodeOf(s)}
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div class="popover menu rmenu" role="menu" tabindex="-1" bind:this={menuEl} style:left="{menu.x}px" style:top="{menu.y}px" onkeydown={menuKeys}>
    <button class="menu-item" role="menuitem" onclick={() => pick((v, p) => (location.hash = serverHref(p, v).slice(1)))}><Server size={14} />Open</button>
    {#if canPowerOn(menu.p) && !(menu.p && dim(menu.p)) && !s.stale && nd?.ceiling !== 'look'}
      <div class="menu-sep"></div>
      {#if !up && s.state !== 'stopping' && nd?.maintenance}
        <button class="menu-item" role="menuitem" disabled title="{nd.name} is under maintenance"><Play size={14} />Start (node paused)</button>
      {:else if !up && s.state !== 'stopping'}
        <button class="menu-item" role="menuitem" onclick={() => pick((v, p) => power(v, 'start', p))}><Play size={14} />Start</button>
      {:else if up}
        <button class="menu-item" role="menuitem" onclick={() => pick((v, p) => power(v, 'restart', p))}><RotateCw size={14} />Restart</button>
        <button class="menu-item" role="menuitem" onclick={() => pick((v, p) => power(v, 'stop', p))}><Square size={14} />Stop</button>
      {/if}
    {/if}
    <div class="menu-sep"></div>
    <button class="menu-item" role="menuitem" onclick={() => pick((v, p) => (location.hash = serverHref(p, v, '/files').slice(1)))}><FolderOpen size={14} />Open folder</button>
    {#if s.addresses?.length}
      <button class="menu-item" role="menuitem" onclick={() => pick(copyAddress)}><Copy size={14} />Copy address</button>
    {/if}
  </div>
{/if}

<style>
  .list { position: relative; flex: none; height: 100%; overflow-y: auto; overflow-x: hidden; display: flex; flex-direction: column; gap: 2px; padding: 14px 8px; border-right: 1px solid var(--line); background: var(--bg); }
  .list.rail { align-items: center; padding: 12px 0; gap: 8px; }
  .hd { display: flex; align-items: center; gap: 2px; padding: 0 4px 8px 8px; }
  .title { flex: 1; min-width: 0; font-size: 16px; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .srv { display: flex; flex-direction: column; gap: 3px; padding: 8px 10px; border-radius: var(--radius-lg); color: var(--text); }
  .line { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .nm { flex: 1; min-width: 0; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .cap { padding-left: 44px; font-size: 11px; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .upd { width: 7px; height: 7px; border-radius: 50%; background: var(--acc); box-shadow: 0 0 0 2px var(--acc-dim); flex: none; }
  /* A server being made: desktop's making row. */
  .making.ready { background: #22c55e14; }
  .mk.bad { color: var(--red); }
  .mk.ok { color: var(--green); }
  .mkbar { margin-left: 44px; height: 3px; border-radius: 2px; background: var(--line2); overflow: hidden; }
  .mkbar i { display: block; height: 100%; background: var(--acc); }
  .mkact { display: flex; gap: 6px; padding-left: 44px; }
  .skels { display: flex; flex-direction: column; }
  /* A panel's group: desktop's server group header (chevron, light, name, count). */
  .gh { display: flex; align-items: center; gap: 6px; min-width: 0; padding: 4px 4px 0 2px; }
  .gfold { display: flex; align-items: center; gap: 6px; min-width: 0; flex: 0 1 auto; background: none; border: 0; padding: 4px 4px; border-radius: var(--radius); color: var(--text); cursor: pointer; font: inherit; }
  .gfold:hover { background: var(--hover, #ededef10); }
  .gfold :global(svg) { color: var(--muted); flex: none; }
  .gnm { font-weight: 600; font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .gfill { flex: 1; }
  .gmore { flex: none; }
  .health { padding: 0 10px 4px 32px; font-size: 11.5px; color: var(--muted); overflow-wrap: anywhere; }
  .health.trouble { color: var(--subtle); }
  /* Inline so it wraps as one sentence; the icon keeps the text's colour
     (the amber light already marks it, and amber fails 3:1 on light). */
  .health.maint { color: var(--subtle); }
  .health.maint :global(svg) { vertical-align: -1px; }
  /* Accent mixed toward the text colour: readable on light themes too (plain --acc is 3.4:1 there). */
  .link { background: none; border: 0; padding: 0; font: inherit; color: color-mix(in srgb, var(--acc) 60%, var(--text)); cursor: pointer; text-decoration: none; }
  .link:hover { text-decoration: underline; }
  .gsep { height: 1px; background: var(--line); margin: 8px 4px 4px; flex: none; }
  .srv.dim, .tile.dim { opacity: .55; }
  .cap.none { padding: 2px 10px 6px 32px; }
  .cap.none.flat { padding-left: 10px; }
  /* Selecting: a tick box leads each row; the row itself is what's pressed. */
  .srv.picking { cursor: default; user-select: none; }
  .ck { width: 15px; height: 15px; margin: 0; flex: none; accent-color: var(--acc); pointer-events: none; }
  .srv.picking .cap { padding-left: 67px; }
  .hint { padding: 0 10px 6px; font-size: 11.5px; color: var(--muted); }
  /* The bulk bar in a list this narrow: "3 selected" and clear on top, the four actions in two columns under it,
     instead of one long row that wraps unevenly. */
  .list :global(.selb) { display: grid; grid-template-columns: 1fr 1fr; align-items: center; gap: 2px; width: 100%; box-sizing: border-box; }
  .list :global(.selb > b) { grid-column: 1; grid-row: 1; padding: 0 6px; }
  .list :global(.selb > .vr) { display: none; }
  .list :global(.selb > button:last-child) { grid-column: 2; grid-row: 1; justify-self: end; }
  .list :global(.selb > button:not(:last-child)) { justify-content: flex-start; }
  /* Filters: which panel, then a segmented state pick (the chosen one in the selected tint). */
  .fbar { display: flex; flex-direction: column; gap: 6px; padding: 0 4px 8px; }
  .fbar .sel { width: 100%; height: 28px; background: var(--base); border: 1px solid var(--line2); border-radius: var(--radius); padding: 0 6px; color: var(--text); font: inherit; font-size: 12.5px; }
  .seg { display: flex; border: 1px solid var(--line2); border-radius: var(--radius); overflow: hidden; }
  .seg button { flex: 1 1 0; min-width: 0; height: 26px; padding: 0 4px; font-size: 12px; color: var(--subtle); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .seg button + button { border-left: 1px solid var(--line2); }
  .seg button:hover:not([aria-pressed='true']) { background: var(--hover); color: var(--text); }
  .seg button[aria-pressed='true'] { background: var(--sel); color: var(--text); font-weight: 600; }
  .seg button:focus-visible { outline: 2px solid var(--acc); outline-offset: -2px; }
  .addp { align-self: flex-start; margin: 10px 0 0 4px; color: var(--subtle); }
  .rsep { width: 28px; height: 1px; background: var(--line); flex: none; }
  .rmenu { position: fixed; z-index: 66; }
  .tile { position: relative; width: 44px; height: 44px; flex: none; display: grid; place-items: center; border: 1px solid var(--line); border-radius: var(--radius-lg); color: var(--text); font-size: 12.5px; font-weight: 600; }
  .tile :global(.tic) { width: 26px; height: 26px; }
  .tdot { position: absolute; top: 3px; right: 3px; }
  .grip { position: absolute; top: 0; right: 0; width: 6px; height: 100%; cursor: col-resize; z-index: 2; }
  .grip:focus-visible { outline: none; }
  .grip:hover, .grip.on, .grip:focus-visible { background: linear-gradient(to right, transparent 2px, var(--acc) 2px, var(--acc) 4px, transparent 4px); }
  :global(body.resizing), :global(body.resizing *) { cursor: col-resize !important; user-select: none !important; }
  @media (max-width: 760px) {
    .list { width: 100% !important; height: auto; max-height: 40vh; border-right: 0; border-bottom: 1px solid var(--line); }
    .list.rail { flex-direction: row; flex-wrap: wrap; padding: 8px 16px; }
    /* Full width here: the four actions fit on one row under the count, so the
       bar covers less of the short list. */
    .list :global(.selb) { grid-template-columns: repeat(4, 1fr); }
    .list :global(.selb > b) { grid-column: 1 / 4; }
    .list :global(.selb > button:last-child) { grid-column: 4; }
    .grip { display: none; }
  }
</style>
