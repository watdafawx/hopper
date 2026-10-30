<script>
  // The server list down the leading side, like Hopper desktop's Servers
  // page: a state light, the name, a pill for running / starting / failing
  // servers (a stopped one is just its grey dot), and a quiet caption.
  // Running servers first. Drag the edge to resize; dragged narrow, it
  // becomes a rail of tiles (desktop's server_rail). Right-click a server
  // for desktop's row menu: Open, Start / Stop / Restart, Open folder.
  import { tick } from 'svelte';
  import { Plus, RefreshCw, PanelLeftClose, PanelLeftOpen, Server, Play, Square, RotateCw, FolderOpen, Copy } from '@lucide/svelte';
  import { power } from './lib/power.js';
  import { say } from './lib/toast.svelte.js';
  import { live } from './lib/live.svelte.js';
  import { packUpdates } from './lib/packs.svelte.js';
  import { stateText, bootText, loaderText } from './lib/format.js';
  import Avatar from './Avatar.svelte';

  let { selected = null, owner = false, canPower = true, onNew, onRescan } = $props();

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
  const loading = $derived(!live.connected && !live.servers.length);

  /** The pill: players while it runs, else its state; none when stopped. */
  function pill(s) {
    if (s.state === 'running') return { text: `${s.players.length}/${s.max_players}`, glow: true };
    if (s.state === 'starting' || s.state === 'stopping' || s.state === 'crashed') return { text: stateText(s), glow: false };
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
  let menu = $state(null); // { s, x, y }
  let menuEl = $state();

  async function openMenu(e, s) {
    e.preventDefault();
    // From the keyboard (the menu key, Shift+F10) there's no pointer: under the row.
    let { clientX: x, clientY: y } = e;
    if (!x && !y) {
      const r = e.currentTarget.getBoundingClientRect();
      x = r.left + 12;
      y = r.bottom;
    }
    menu = { s, x, y };
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
    const s = menu?.s;
    closeMenu();
    if (s) f(s);
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

  const href = (s) => `#/server/${encodeURIComponent(s.id)}`;
</script>

<aside class="list" class:rail={collapsed} style:width="{collapsed ? RAIL : width}px">
  {#if collapsed}
    <button class="btn ghost icon" title="Show server list" onclick={toggle}><PanelLeftOpen size={15} /></button>
    {#each servers as s (s.id)}
      <a class="tile" class:row-sel={selected === s.id} class:row-hover={selected !== s.id} href={href(s)} title={s.name} oncontextmenu={(e) => openMenu(e, s)}>
        {#if s.icon}<Avatar server={s} cls="sic tic" />{:else}{initials(s.name)}{/if}
        <span class="dot {s.state} tdot"></span>
      </a>
    {/each}
  {:else}
    <div class="hd">
      <span class="title">Servers</span>
      {#if owner}<button class="btn ghost icon" title="Look for new server folders" onclick={onRescan}><RefreshCw size={14} /></button>{/if}
      <button class="btn ghost icon" title="New server" onclick={onNew}><Plus size={15} /></button>
      <button class="btn ghost icon" title="Hide server list" onclick={toggle}><PanelLeftClose size={15} /></button>
    </div>
    {#if loading}
      <div class="skels">{#each [0, 1, 2] as i (i)}<div class="skel"><i></i></div>{/each}</div>
    {/if}
    {#each servers as s (s.id)}
      {@const p = pill(s)}
      {@const cap = caption(s)}
      {@const up = packUpdates.byId[s.id]}
      <a class="srv" class:row-sel={selected === s.id} class:row-hover={selected !== s.id} href={href(s)} oncontextmenu={(e) => openMenu(e, s)}>
        <div class="line">
          <span class="dot {s.state}"></span>
          <Avatar server={s} cls="sic" />
          <span class="nm">{s.name}</span>
          {#if up}<span class="upd" title="Pack update: {up.from} → {up.to}"></span>{/if}
          {#if p}<span class="pill {s.state}" title={bootText(s) || undefined}>{#if p.glow}<span class="glow"></span>{/if}{p.text}</span>{/if}
        </div>
        {#if cap}<div class="cap">{cap}</div>{/if}
      </a>
    {:else}
      {#if !loading}
        <div class="empty-state">
          <span class="es-ic"><Server size={18} /></span>
          <b>No servers yet</b>
          <p>Make one with New server{owner ? ', or put server folders in the panel folder' : ''}.</p>
          <button class="btn sm pri" onclick={onNew}><Plus size={13} />New server</button>
        </div>
      {/if}
    {/each}
  {/if}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div class="grip" class:on={drag} role="separator" aria-orientation="vertical" aria-label="Server list width" aria-valuenow={collapsed ? RAIL : width} aria-valuemin={RAIL} aria-valuemax={MAX} tabindex="0" title="Drag to resize" onpointerdown={down} ondblclick={toggle} onkeydown={gripKey}></div>
</aside>

<svelte:window onpointerdown={away} onresize={closeMenu} onblur={closeMenu} />

{#if menu}
  {@const s = menu.s}
  {@const up = s.state === 'running' || s.state === 'starting'}
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div class="popover menu rmenu" role="menu" tabindex="-1" bind:this={menuEl} style:left="{menu.x}px" style:top="{menu.y}px" onkeydown={menuKeys}>
    <button class="menu-item" role="menuitem" onclick={() => pick((v) => (location.hash = `/server/${encodeURIComponent(v.id)}`))}><Server size={14} />Open</button>
    {#if canPower}
      <div class="menu-sep"></div>
      {#if !up && s.state !== 'stopping'}
        <button class="menu-item" role="menuitem" onclick={() => pick((v) => power(v, 'start'))}><Play size={14} />Start</button>
      {:else if up}
        <button class="menu-item" role="menuitem" onclick={() => pick((v) => power(v, 'restart'))}><RotateCw size={14} />Restart</button>
        <button class="menu-item" role="menuitem" onclick={() => pick((v) => power(v, 'stop'))}><Square size={14} />Stop</button>
      {/if}
    {/if}
    <div class="menu-sep"></div>
    <button class="menu-item" role="menuitem" onclick={() => pick((v) => (location.hash = `/server/${encodeURIComponent(v.id)}/files`))}><FolderOpen size={14} />Open folder</button>
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
  .skels { display: flex; flex-direction: column; }
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
