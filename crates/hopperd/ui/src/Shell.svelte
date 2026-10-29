<script>
  // The app shell, laid out like Hopper desktop (src/gpui/app.rs): a title
  // bar with the logo, back / forward, the page tabs (underlined when on),
  // then Tasks, a tools track (Ctrl K, re-read, Settings) and the account
  // chip. Under it, the server list down the side (desktop's Servers page
  // list), or Settings with its own category list.
  import { onMount, onDestroy } from 'svelte';
  import {
    LayoutDashboard, Compass, ChevronLeft, ChevronRight, Search, RefreshCw, Settings as Gear, LogOut, Wifi, WifiOff, Globe,
    Waypoints, Plus, Play, Square, RotateCw, Server, Terminal, Folder, Puzzle, Earth, Users, Archive, CalendarClock, SlidersHorizontal,
    CircleCheck, CircleAlert, FolderOpen, X as Close,
  } from '@lucide/svelte';
  import { boot } from './lib/boot.svelte.js';
  import { api, post } from './lib/api.js';
  import { live, connect, disconnect, startPing } from './lib/live.svelte.js';
  import { pathKind } from './lib/format.js';
  import { toast, say, hush } from './lib/toast.svelte.js';
  import { power } from './lib/power.js';
  import Overview from './Overview.svelte';
  import Browse from './Browse.svelte';
  import ServerPage from './ServerPage.svelte';
  import PanelSettings, { catsFor } from './PanelSettings.svelte';
  import ServerList from './ServerList.svelte';
  import Tasks from './Tasks.svelte';
  import Palette from './Palette.svelte';
  import NewServer from './NewServer.svelte';
  import { session, ownerLike, roleLabel } from './lib/me.svelte.js';
  import { dialog } from './lib/dialog.svelte.js';
  import { leave } from './lib/leave.js';
  import Dialog from './Dialog.svelte';
  import Transfers from './Transfers.svelte';
  import { refreshPackUpdates } from './lib/packs.svelte.js';
  import VersionChip from './VersionChip.svelte';
  import PanelVersions from './PanelVersions.svelte';
  import { upd } from './lib/panelupdate.svelte.js';

  let { hello, me, onSignOut } = $props();
  const owner = $derived(ownerLike(me));
  const canPower = $derived(me?.role !== 'viewer');
  $effect(() => {
    session.owner = owner;
    session.trueOwner = me?.role === 'owner';
    session.home = me?.panel_folder || '';
  });

  let route = $state(location.hash.slice(1) || '/');
  let pending = $state([]);
  let making = $state(false);
  let palette = $state(null);
  let menu = $state(false);
  let menuEl = $state();
  let pingTimer, pendTimer, packTimer, packSoon;

  // Back / forward: each entry this tab made in the panel carries its place
  // (history.state.hopperNav), so the arrows grey out at either end instead
  // of leaving the panel.
  const NAVKEY = 'hopperd.nav.top';
  let navPos = $state(0);
  let navTop = $state(0);
  const navState = (i) => ({ ...(history.state && typeof history.state === 'object' ? history.state : {}), hopperNav: i });
  function saveTop() {
    try {
      sessionStorage.setItem(NAVKEY, String(navTop));
    } catch {
      // storage off: forward greys out after a reload
    }
  }
  function navInit() {
    const at = history.state?.hopperNav;
    if (typeof at === 'number') {
      navPos = at;
      try {
        navTop = Math.max(at, Number(sessionStorage.getItem(NAVKEY)) || 0);
      } catch {
        navTop = at;
      }
    } else {
      history.replaceState(navState(0), '');
      saveTop();
    }
  }
  function navMoved() {
    const at = history.state?.hopperNav;
    if (typeof at === 'number') navPos = at;
    else {
      // A new entry (a link, go()): everything after it is gone.
      navPos += 1;
      navTop = navPos;
      history.replaceState(navState(navPos), '');
      saveTop();
    }
  }

  /** Older links (#/access, #/panel-folder) are Settings categories now: put the new one in the address bar. */
  function canonical(r) {
    if (r === '/access') return owner ? '/settings/access' : '/settings/account';
    if ((r === '/panel-folder' || r === '/servers-folder') && owner) return '/settings/panel-folder';
    return null;
  }

  // Older links: #/access and #/panel-folder are Settings categories now.
  const settingsCat = $derived(
    route.startsWith('/settings') ? route.split('/')[2] || ''
      : route === '/access' ? (owner ? 'access' : 'account')
      : (route === '/panel-folder' || route === '/servers-folder') && owner ? 'panel-folder' : null,
  );
  const page = $derived(route.startsWith('/server/') ? 'server' : route === '/browse' || route.startsWith('/browse/') ? 'browse' : settingsCat != null ? 'settings' : 'overview');
  // #/server/<id>/<tab>/<path...>
  const segs = $derived(route.split('/').slice(2).map(decodeURIComponent));
  const serverId = $derived(page === 'server' ? segs[0] : null);
  const tab = $derived(page === 'server' ? segs[1] || 'console' : null);
  const subpath = $derived(page === 'server' ? segs.slice(2).join('/') : '');
  const kind = pathKind();
  const PathIcon = { LAN: Wifi, Local: Wifi, Tailscale: Waypoints, Internet: Globe }[kind];
  const NAV = [
    { id: 'overview', label: 'Overview', icon: LayoutDashboard, to: '/' },
    { id: 'browse', label: 'Browse', icon: Compass, to: '/browse' },
  ];
  const TABS = [
    ['console', 'Console', Terminal], ['files', 'Files', Folder], ['mods', 'Mods', Puzzle], ['worlds', 'Worlds', Earth],
    ['players', 'Players', Users], ['backups', 'Backups', Archive], ['schedule', 'Schedule', CalendarClock], ['settings', 'Settings', SlidersHorizontal],
  ];

  function go(r) {
    location.hash = r;
  }

  async function checkPending() {
    if (document.hidden) return;
    try {
      pending = await api('/pair/pending');
    } catch {
      // devices can't list; ignore
    }
  }

  async function signOut() {
    await post('/auth/logout').catch(() => {});
    disconnect();
    onSignOut();
  }

  async function rescan() {
    try {
      const r = await post('/servers/rescan');
      say(`Found ${r.servers} server${r.servers === 1 ? '' : 's'}.`);
    } catch (e) {
      say(e.message, true);
    }
  }

  /** What the palette offers, as of opening it. */
  function paletteItems() {
    const v = [];
    const here = live.servers.find((s) => s.id === serverId);
    v.push({ id: 'new', section: 'Actions', icon: Plus, title: 'New server…', words: 'create host make add', run: () => (making = true) });
    if (here) {
      const up = here.state === 'running' || here.state === 'starting';
      if (!canPower) {
        // A viewer can't start or stop it.
      } else if (!up) {
        if (here.state !== 'stopping') v.push({ id: 'start', section: 'Actions', icon: Play, title: `Start ${here.name}`, words: 'run launch power', run: () => power(here, 'start') });
      } else {
        v.push({ id: 'restart', section: 'Actions', icon: RotateCw, title: `Restart ${here.name}`, words: 'reboot power', run: () => power(here, 'restart') });
        v.push({ id: 'stop', section: 'Actions', icon: Square, title: `Stop ${here.name}`, words: 'shut down power', run: () => power(here, 'stop') });
      }
      const at = `/server/${encodeURIComponent(here.id)}`;
      v.push({ id: 'folder', section: 'Actions', icon: FolderOpen, title: `Open ${here.name}'s folder`, words: 'files explorer browse', run: () => go(`${at}/files`) });
    }
    if (owner) v.push({ id: 'rescan', section: 'Actions', icon: RefreshCw, title: 'Look for new server folders', words: 'refresh reload rescan re-read', run: rescan });
    v.push({ id: 'signout', section: 'Actions', icon: LogOut, title: 'Sign out', words: 'log out logout leave', run: signOut });
    for (const n of NAV) v.push({ id: `nav:${n.id}`, section: 'Navigation', icon: n.icon, title: n.label, run: () => go(n.to) });
    for (const c of catsFor(owner)) {
      v.push({ id: `set:${c.id}`, section: 'Navigation', icon: c.icon, title: `Settings › ${c.label}`, words: c.words, run: () => go(`/settings/${c.id}`) });
    }
    for (const s of live.servers) {
      const at = `/server/${encodeURIComponent(s.id)}`;
      v.push({ id: `srv:${s.id}`, section: 'Servers', icon: Server, title: `Servers › ${s.name}`, hint: [s.loader, s.version].filter(Boolean).join(' '), run: () => go(at) });
      for (const [t, label, icon] of TABS) {
        v.push({ id: `srv:${s.id}:${t}`, section: 'Servers', icon, title: `Servers › ${s.name} › ${label}`, deep: true, run: () => go(`${at}/${t}`) });
      }
    }
    return v;
  }

  function openPalette() {
    menu = false;
    palette = palette ? null : { items: paletteItems(), context: live.servers.find((s) => s.id === serverId)?.name || '' };
  }

  /** The next or previous server in the list (Ctrl+Up / Ctrl+Down). */
  function step(d) {
    const alive = (s) => s.state === 'running' || s.state === 'starting' || s.state === 'stopping';
    const list = [...live.servers].sort((a, b) => Number(alive(b)) - Number(alive(a)));
    if (!list.length) return;
    const i = list.findIndex((s) => s.id === serverId);
    const j = i < 0 ? (d > 0 ? 0 : list.length - 1) : (i + d + list.length) % list.length;
    const n = list[j];
    go(`/server/${encodeURIComponent(n.id)}${tab && tab !== 'console' ? `/${tab}` : ''}`);
  }

  function typing(e) {
    const t = e.target;
    return t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));
  }

  /** In a code editor (CodeMirror) or other rich text: its own keys win. */
  const inEditor = (e) => !!(e.target?.isContentEditable || e.target?.closest?.('.cm-editor'));

  // App-wide keys, as on desktop. Nothing while a dialog asks something.
  function keys(e) {
    if (dialog.open || e.defaultPrevented) return;
    const mod = e.ctrlKey || e.metaKey;
    if (mod && !e.shiftKey && !e.altKey && (e.key === 'k' || e.key === 'K' || e.key === 'p' || e.key === 'P')) {
      if (inEditor(e) && !palette) return;
      e.preventDefault();
      if (!e.repeat) openPalette();
    } else if (mod && e.key === ',') {
      e.preventDefault();
      go('/settings');
    } else if (mod && (e.key === 'ArrowUp' || e.key === 'ArrowDown') && !typing(e)) {
      e.preventDefault();
      step(e.key === 'ArrowDown' ? 1 : -1);
    } else if (e.key === 'Escape' && menu) {
      menu = false;
    }
  }

  function outside(e) {
    if (menu && menuEl && !menuEl.contains(e.target)) menu = false;
  }

  onMount(() => {
    navInit();
    const fix = () => {
      const c = canonical(location.hash.slice(1) || '/');
      if (c) history.replaceState(navState(navPos), '', `#${c}`);
      route = location.hash.slice(1) || '/';
    };
    fix();
    // A page with unsaved work (the editor) can ask before the panel leaves
    // it: put the old address back, ask, then go on if it says so.
    let restoring = false;
    const onHash = async () => {
      if (restoring) {
        restoring = false;
        return;
      }
      const to = location.hash.slice(1) || '/';
      const g = leave.guard;
      if (g?.blocked() && to !== route) {
        const at = history.state?.hopperNav;
        restoring = true;
        if (typeof at === 'number' && at !== navPos) history.go(navPos - at);
        else history.back();
        if (await g.settle()) location.hash = to;
        return;
      }
      navMoved();
      fix();
    };
    addEventListener('hashchange', onHash);
    addEventListener('keydown', keys);
    addEventListener('pointerdown', outside);
    connect();
    pingTimer = startPing();
    // Servers with a newer modpack version: the panel checks stale ones in
    // the background, so look again shortly after the first call.
    refreshPackUpdates();
    packSoon = setTimeout(refreshPackUpdates, 45000);
    packTimer = setInterval(refreshPackUpdates, 15 * 60 * 1000);
    if (owner) {
      checkPending();
      pendTimer = setInterval(checkPending, 3000);
    }
    return () => {
      removeEventListener('hashchange', onHash);
      removeEventListener('keydown', keys);
      removeEventListener('pointerdown', outside);
    };
  });

  onDestroy(() => {
    clearInterval(pingTimer);
    clearInterval(pendTimer);
    clearInterval(packTimer);
    clearTimeout(packSoon);
  });
</script>

<div class="win">
  <header class="tbar">
    <div class="brand">
      <a class="home" href="#/" title="Overview"><img class="logo" src="/logo.svg" alt="" /><span class="bname">Hopper Server</span></a>
      <VersionChip />
      <span class="chip tiny host" title="This panel's PC">{hello.name}</span>
    </div>
    <div class="arrows">
      <button class="btn ghost icon" title="Back (Alt+Left)" disabled={navPos <= 0} onclick={() => history.back()}><ChevronLeft size={15} /></button>
      <button class="btn ghost icon" title="Forward (Alt+Right)" disabled={navPos >= navTop} onclick={() => history.forward()}><ChevronRight size={15} /></button>
    </div>
    <nav class="tabs">
      {#each NAV as n (n.id)}
        {@const Icon = n.icon}
        <a class="tab" class:tab-sel={page === n.id} href="#{n.to}"><Icon size={15} color={page === n.id ? 'var(--acc)' : 'currentColor'} /><span>{n.label}</span></a>
      {/each}
    </nav>
    <span class="sp"></span>
    <div class="tools">
      {#if pending.length && settingsCat !== 'access'}
        <button class="chip acc pair" onclick={() => go('/settings/access')}>{pending[0].device} wants to pair</button>
      {/if}
      <span class="conn" class:off={!live.connected} title="How this browser reaches the panel{live.ping != null && live.connected ? ` · ${live.ping} ms` : ''}">
        {#if live.connected}<PathIcon size={13} />{:else}<WifiOff size={13} />{/if}
        <span class="ct">{#if location.protocol === 'https:'}🔒 {/if}{live.connected ? kind : 'Reconnecting…'}</span>
      </span>
      {#if owner}<Tasks />{/if}
      <div class="track">
        <button class="btn ghost sm" title="Search or run a command (Ctrl+K)" onclick={openPalette}><Search size={14} /><span class="kl">Ctrl K</span></button>
        {#if owner}<button class="btn ghost icon" title="Look for new server folders" onclick={rescan}><RefreshCw size={14} /></button>{/if}
        <button class="btn ghost icon" class:on={page === 'settings'} title="Settings (Ctrl+,)" onclick={() => go('/settings')}>
          <Gear size={14} />{#if pending.length}<span class="pdot"></span>{/if}
        </button>
      </div>
      <div class="account" bind:this={menuEl}>
        <button class="who row-hover" title="Your account" onclick={() => (menu = !menu)}>
          <span class="nm">{me?.name || 'owner'}</span><span class="face">{(me?.name || 'o')[0].toUpperCase()}</span>
        </button>
        {#if menu}
          <div class="popover menu dd">
            <div class="mh"><b>{me?.name || 'owner'}</b><span class="tiny muted">{roleLabel(me?.role)}{owner ? '' : me?.servers?.length ? ` · ${me.servers.length} server${me.servers.length === 1 ? '' : 's'}` : ''}</span></div>
            <a class="menu-item" href="#/settings" onclick={() => (menu = false)}><Gear size={14} />{owner ? 'Settings' : 'Account'}</a>
            <div class="menu-sep"></div>
            <button class="menu-item" onclick={signOut}><LogOut size={14} />Sign out</button>
            <div class="menu-sep"></div>
            <div class="tiny muted ver">Hopper Server {hello.version}</div>
          </div>
        {/if}
      </div>
    </div>
  </header>
  {#if boot.updated != null}
    <!-- The panel came back on a new version while something was open here:
         the page waits for the user instead of reloading over it. -->
    <div class="updated" role="status">
      <RotateCw size={13} />
      <span>Hopper Server updated{boot.updated ? ` to ${boot.updated}` : ''}. Reload to use the new version.</span>
      <button class="btn pri sm" onclick={() => location.reload()}>Reload</button>
      <button class="btn ghost icon" title="Later" aria-label="Later" onclick={() => (boot.updated = null)}><Close size={13} /></button>
    </div>
  {/if}
  <div class="body">
    {#if page !== 'settings'}
      <ServerList selected={serverId} {owner} {canPower} onNew={() => (making = true)} onRescan={rescan} />
    {/if}
    <main class="content">
      {#if page === 'browse'}
        <Browse segs={segs} />
      {:else if page === 'server'}
        {#key serverId}<ServerPage id={serverId} {tab} path={subpath} />{/key}
      {:else if page === 'settings'}
        <PanelSettings cat={settingsCat} {hello} {me} bind:pending />
      {:else}
        <Overview onNew={() => (making = true)} />
      {/if}
    </main>
  </div>
</div>

{#if making}<NewServer close={() => (making = false)} />{/if}
{#if upd.open}<PanelVersions />{/if}
{#if palette}<Palette items={palette.items} context={palette.context} onClose={() => (palette = null)} />{/if}
<Transfers />
<Dialog />
<!-- Always there, so screen readers hear each message; a click puts it away. -->
<div role="status" aria-live="polite" aria-atomic="true">
  {#if toast.text}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="toast" class:bad={toast.bad} onclick={hush}>
      {#if toast.bad}<CircleAlert size={15} color="var(--red)" />{:else}<CircleCheck size={15} color="var(--green)" />{/if}
      <span>{toast.text}</span>
    </div>
  {/if}
</div>

<style>
  .win { height: 100%; display: flex; flex-direction: column; }
  .updated { flex: none; display: flex; align-items: center; gap: 10px; padding: 5px 8px 5px 14px; font-size: 12.5px; color: var(--blue); background: var(--blue-dim); border-bottom: 1px solid var(--side-line); }
  .updated span { color: var(--text); }
  .tbar { height: var(--titlebar); flex: none; display: flex; align-items: center; gap: 14px; padding: 0 8px 0 14px; background: var(--side); border-bottom: 1px solid var(--side-line); }
  .brand { display: flex; align-items: center; gap: 8px; flex: none; }
  .home { display: flex; align-items: center; gap: 8px; color: inherit; text-decoration: none; border-radius: 6px; }  .logo { width: 20px; height: 20px; image-rendering: pixelated; flex: none; }
  .bname { font-weight: 600; font-size: 13px; white-space: nowrap; }
  .host { max-width: 110px; overflow: hidden; text-overflow: ellipsis; }
  .arrows { display: flex; gap: 2px; }
  .tabs { display: flex; align-items: center; gap: 2px; height: 100%; }
  .tab { display: flex; align-items: center; gap: 6px; height: 30px; padding: 0 12px; border-radius: var(--radius); color: var(--subtle); font-size: 13px; }
  .tab:hover:not(.tab-sel) { background: var(--hover); color: var(--text); }
  .tools { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .track { display: flex; align-items: center; gap: 2px; padding: 2px; border: 1px solid var(--line); border-radius: 10px; }
  .track .btn { position: relative; }
  .kl { color: var(--muted); font-size: 11.5px; }
  .pdot { position: absolute; top: 3px; right: 3px; width: 6px; height: 6px; border-radius: 50%; background: var(--acc); }
  .conn { display: flex; align-items: center; gap: 6px; font-size: 11.5px; color: var(--muted); white-space: nowrap; }
  .conn.off { color: var(--gold); }
  .account { position: relative; }
  .who { display: flex; align-items: center; gap: 8px; padding: 2px 4px 2px 8px; border-radius: var(--radius); }
  .who .nm { font-size: 11.5px; font-weight: 600; max-width: 120px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .face { width: 20px; height: 20px; border-radius: 5px; display: grid; place-items: center; background: var(--raise); color: var(--subtle); font-size: 10.5px; font-weight: 700; }
  .dd { position: absolute; right: 0; top: calc(100% + 6px); z-index: 66; min-width: 220px; }
  .mh { display: flex; flex-direction: column; padding: 6px 10px 4px; }
  .ver { padding: 4px 10px 6px; }
  .body { flex: 1; min-height: 0; display: flex; }
  .content { flex: 1; min-width: 0; min-height: 0; overflow: auto; display: flex; flex-direction: column; }
  @media (max-width: 760px) {
    .tbar { gap: 8px; padding: 0 8px; overflow-x: auto; }
    .bname, .host, .arrows, .conn, .pair, .kl, .who .nm, .tab span { display: none; }
    .tbar { scrollbar-width: none; }
    .tab { padding: 0 9px; }
    .body { flex-direction: column; }
  }
</style>
