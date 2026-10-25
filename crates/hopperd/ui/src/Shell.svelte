<script>
  import Avatar from './Avatar.svelte';
  import { onMount, onDestroy } from 'svelte';
  import { Server, LayoutDashboard, Shield, LogOut, Wifi, WifiOff, Globe, Waypoints, RefreshCw } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { live, connect, disconnect, startPing } from './lib/live.svelte.js';
  import { color, pathKind } from './lib/format.js';
  import { toast, say } from './lib/toast.svelte.js';
  import Overview from './Overview.svelte';
  import ServerPage from './ServerPage.svelte';
  import Access from './Access.svelte';
  import Dialog from './Dialog.svelte';
  import Transfers from './Transfers.svelte';

  let { hello, me, onSignOut } = $props();
  const owner = $derived(me?.role === 'owner');

  let route = $state(location.hash.slice(1) || '/');
  let pending = $state([]);
  let pingTimer, pendTimer;

  const page = $derived(route.startsWith('/server/') ? 'server' : route === '/access' ? 'access' : 'overview');
  // #/server/<id>/<tab>/<path...>
  const segs = $derived(route.split('/').slice(2).map(decodeURIComponent));
  const serverId = $derived(page === 'server' ? segs[0] : null);
  const tab = $derived(page === 'server' ? segs[1] || 'console' : null);
  const subpath = $derived(page === 'server' ? segs.slice(2).join('/') : '');
  const kind = pathKind();
  const PathIcon = { LAN: Wifi, Local: Wifi, Tailscale: Waypoints, Internet: Globe }[kind];

  function go(r) {
    location.hash = r;
  }

  async function checkPending() {
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

  onMount(() => {
    const onHash = () => (route = location.hash.slice(1) || '/');
    addEventListener('hashchange', onHash);
    connect();
    pingTimer = startPing();
    if (owner) {
      checkPending();
      pendTimer = setInterval(checkPending, 3000);
    }
    return () => removeEventListener('hashchange', onHash);
  });

  onDestroy(() => {
    clearInterval(pingTimer);
    clearInterval(pendTimer);
  });
</script>

<div class="win">
  <aside class="side">
    <div class="brand">
      <div class="logo"><Server size={15} strokeWidth={2.5} /></div>
      Hopper Server
      <span class="chip tiny host">{hello.name}</span>
    </div>
    <button class="nav" class:on={page === 'overview'} onclick={() => go('/')}><LayoutDashboard size={14} />Overview</button>
    <div class="navsec lbl row">Servers<span class="sp"></span>
      {#if owner}<button class="btn ghost icon" title="Look for new server folders" onclick={rescan}><RefreshCw size={12} /></button>{/if}
    </div>
    {#each live.servers as s (s.id)}
      <button class="srv" class:on={serverId === s.id} onclick={() => go(`/server/${encodeURIComponent(s.id)}`)}>
        <Avatar server={s} cls="sic" />
        <span class="nm">{s.name}</span>
        <span class="dot {s.state}"></span>
      </button>
    {:else}
      <div class="tiny muted pad">No servers yet. Start hopperd with <span class="mono">--servers-root</span>.</div>
    {/each}
    <div class="foot">
      <button class="nav" class:on={page === 'access'} onclick={() => go('/access')}>
        <Shield size={14} />{owner ? 'Access & devices' : `Account · ${me?.name}`}
        {#if pending.length}<span class="badge">{pending.length}</span>{/if}
      </button>
      <button class="nav" onclick={signOut}><LogOut size={14} />Sign out</button>
      <div class="tiny muted pad">hopperd {hello.version}</div>
    </div>
  </aside>
  <div class="main">
    <header class="top">
      {#if pending.length && page !== 'access'}
        <button class="chip acc" onclick={() => go('/access')}>{pending[0].device} wants to pair</button>
      {/if}
      <span class="sp"></span>
      <span class="conn" class:off={!live.connected} title="How this browser reaches the panel">
        {#if live.connected}<PathIcon size={13} />{:else}<WifiOff size={13} />{/if}
        {#if location.protocol === 'https:'}🔒 {/if}{live.connected ? kind : 'Reconnecting…'} · {location.hostname}{live.ping != null && live.connected ? ` · ${live.ping} ms` : ''}
      </span>
    </header>
    <div class="content">
      {#if page === 'server'}
        {#key serverId}<ServerPage id={serverId} {tab} path={subpath} />{/key}
      {:else if page === 'access'}
        <Access {hello} {me} bind:pending />
      {:else}
        <Overview />
      {/if}
    </div>
  </div>
</div>

<Transfers />
<Dialog />
{#if toast.text}<div class="toast" class:bad={toast.bad}>{toast.text}</div>{/if}

<style>
  .win { height: 100%; display: flex; }
  .side { width: 224px; flex: none; border-right: 1px solid var(--line); display: flex; flex-direction: column; padding: 12px 10px; gap: 2px; background: var(--side); overflow: auto; }
  .brand { display: flex; align-items: center; gap: 9px; padding: 4px 8px 14px; font-weight: 700; font-size: 14px; }
  .logo { width: 26px; height: 26px; border-radius: 7px; background: linear-gradient(135deg, #fb923c, #ea580c); display: grid; place-items: center; color: #1a1a1f; }
  .host { margin-left: auto; max-width: 70px; overflow: hidden; text-overflow: ellipsis; }
  .nav { display: flex; align-items: center; gap: 9px; padding: 7px 9px; border-radius: 7px; color: var(--subtle); font-weight: 500; text-align: left; width: 100%; }
  .nav:hover, .srv:hover { background: var(--surface); }
  .nav.on, .srv.on { background: var(--raise); color: var(--text); }
  .navsec { padding: 14px 4px 5px 9px; }
  .srv { display: flex; align-items: center; gap: 9px; padding: 6px 9px; border-radius: 7px; color: var(--subtle); width: 100%; text-align: left; }
  .srv .nm { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pad { padding: 4px 9px; }
  .badge { margin-left: auto; background: var(--acc); color: #1a1a1f; border-radius: 99px; font-size: 11px; font-weight: 700; padding: 0 6px; }
  .foot { margin-top: auto; border-top: 1px solid var(--line); padding-top: 10px; display: flex; flex-direction: column; gap: 2px; }
  .main { flex: 1; min-width: 0; display: flex; flex-direction: column; }
  .top { display: flex; align-items: center; gap: 10px; padding: 10px 18px; border-bottom: 1px solid var(--line); flex: none; min-height: 47px; }
  .conn { display: flex; align-items: center; gap: 7px; padding: 4px 10px; border-radius: 99px; border: 1px solid #60a5fa44; background: var(--blue-dim); color: var(--blue); font-size: 12px; font-weight: 500; white-space: nowrap; }
  .conn.off { border-color: #eab30844; background: var(--gold-dim); color: var(--gold); }
  .content { flex: 1; min-height: 0; overflow: auto; display: flex; flex-direction: column; }
  @media (max-width: 760px) {
    .win { flex-direction: column; }
    .side { width: 100%; flex-direction: row; flex-wrap: wrap; border-right: 0; border-bottom: 1px solid var(--line); padding: 8px 16px; max-height: none; }
    .brand { padding: 4px 8px; width: 100%; }
    .navsec, .foot .pad { display: none; }
    .nav, .srv { width: auto; }
    .foot { margin: 0; border: 0; padding: 0; flex-direction: row; }
    .top { padding: 8px 16px; }
  }
</style>
