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
  import { tick } from 'svelte';
  import { Plus, RefreshCw, PanelLeftClose, PanelLeftOpen, Server, Play, Square, RotateCw, FolderOpen, Copy, Compass, ChevronRight, ChevronDown, Ellipsis, ExternalLink, LogOut, Trash2 } from '@lucide/svelte';
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
  const grouped = $derived(panels.list.length > 0);
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
  {@const sel = isSel(p, s)}
  <a class="srv" class:dim={p && dim(p)} class:row-sel={sel} class:row-hover={!sel} href={serverHref(p, s)} oncontextmenu={(e) => openMenu(e, s, p)}>
    <div class="line">
      <span class="dot {p && dim(p) ? '' : stateKey(s)}"></span>
      <Avatar server={s} cls="sic" panel={p} />
      <span class="nm">{s.name}</span>
      {#if up}<span class="upd" title="Pack update: {up.from} → {up.to}"></span>{/if}
      {#if pl && !(p && dim(p))}<span class="pill {stateKey(s)}" title={bootText(s) || stateTip(s)}>{#if pl.glow}<span class="glow"></span>{/if}{pl.text}</span>{/if}
    </div>
    {#if p && dim(p)}<div class="cap">{p.lastSeen ? `Seen ${ago(p.lastSeen)}` : 'Not seen yet'}</div>
    {:else if s.progress}<Progress server={s} compact />{:else if cap}<div class="cap">{cap}</div>{/if}
  </a>
{/snippet}

{#snippet head(id, name, count, role, lightKey, p)}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div class="gh" role="group" aria-label={name} oncontextmenu={p ? (e) => openMenu(e, null, null, p) : undefined}>
    <button class="gfold" aria-expanded={!fold[id]} title={fold[id] ? 'Show its servers' : 'Hide its servers'} onclick={() => flip(id)}>
      {#if fold[id]}<ChevronRight size={14} />{:else}<ChevronDown size={14} />{/if}
      <span class="dot {lightKey}"></span>
      <span class="gnm">{name}</span>
    </button>
    <span class="gcount">Hopper Server · {count}</span>
    {#if role}<span class="chip">{roleLabel(role)}</span>{/if}
    {#if p}<button class="btn ghost icon sm gmore" title="More" aria-label="More for {name}" onclick={(e) => openMenu(e, null, null, p)}><Ellipsis size={14} /></button>{/if}
  </div>
{/snippet}

<aside class="list" class:rail={collapsed} style:width="{collapsed ? RAIL : width}px">
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
      <button class="btn ghost icon" title="Hide server list" onclick={toggle}><PanelLeftClose size={15} /></button>
    </div>
    {#if grouped}
      {@render head('home', homeName || 'This panel', live.servers.length, homeRole, live.connected ? 'running' : 'starting', null)}
      <div class="health">This panel{homeVersion ? ` · ${homeVersion}` : ''}</div>
    {/if}
    {#if !grouped || !fold.home}
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
      {#each shown as s (s.id)}
        {@render row(s, null)}
      {:else}
        {#if !loading && !making.length}
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

    {#each panels.list as p (p.id)}
      <div class="gsep"></div>
      {@render head(p.id, p.name, p.servers.length, p.me ? p.role : '', light(p), p)}
      {#if p.state === 'ok'}
        <div class="health">{p.version || 'Hopper Server'}{p.ping != null ? ` · ${p.ping} ms` : ''}</div>
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
        {#each sorted(p.servers) as s (s.id)}
          {@render row(s, p)}
        {:else}
          {#if p.state === 'ok'}<div class="cap none">No servers on it yet.</div>{/if}
        {/each}
      {/if}
    {/each}

    {#if canAdd}
      <button class="btn sm ghost addp" onclick={() => (adding = true)}><Plus size={13} />Add a Hopper Server</button>
    {/if}
  {/if}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div class="grip" class:on={drag} role="separator" aria-orientation="vertical" aria-label="Server list width" aria-valuenow={collapsed ? RAIL : width} aria-valuemin={RAIL} aria-valuemax={MAX} tabindex="0" title="Drag to resize" onpointerdown={down} ondblclick={toggle} onkeydown={gripKey}></div>
</aside>

<svelte:window onpointerdown={away} onresize={closeMenu} onblur={closeMenu} />

{#if adding}<AddPanel onClose={() => (adding = false)} />{/if}

{#if menu?.g}
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
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div class="popover menu rmenu" role="menu" tabindex="-1" bind:this={menuEl} style:left="{menu.x}px" style:top="{menu.y}px" onkeydown={menuKeys}>
    <button class="menu-item" role="menuitem" onclick={() => pick((v, p) => (location.hash = serverHref(p, v).slice(1)))}><Server size={14} />Open</button>
    {#if canPowerOn(menu.p) && !(menu.p && dim(menu.p))}
      <div class="menu-sep"></div>
      {#if !up && s.state !== 'stopping'}
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
  .gcount { font-size: 11.5px; color: var(--muted); white-space: nowrap; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .gmore { flex: none; }
  .health { padding: 0 10px 4px 32px; font-size: 11.5px; color: var(--muted); overflow-wrap: anywhere; }
  .health.trouble { color: var(--subtle); }
  .link { background: none; border: 0; padding: 0; font: inherit; color: var(--acc); cursor: pointer; text-decoration: none; }
  .link:hover { text-decoration: underline; }
  .gsep { height: 1px; background: var(--line); margin: 8px 4px 4px; flex: none; }
  .srv.dim, .tile.dim { opacity: .55; }
  .cap.none { padding: 2px 10px 6px 32px; }
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
    .grip { display: none; }
  }
</style>
