<script>
  // The Players tab, as Hopper desktop shows a Hopper Server's players
  // (panel_tabs/players.rs): one column of sections (online now, the
  // operators, the whitelist with a box to add names, the bans), each row
  // with its actions. Kicks and bans ask first, with an optional reason.
  // Refreshes every 10 seconds and after each change.
  import { Crown, Ban, ShieldCheck, UserPlus, Undo2, LogOut, Users } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { askKickOrBan, playerAct, head } from './lib/consoleKit.js';

  let { server } = $props();
  // The live feed hands over a new `server` object every second; effects
  // that should run once per server key on the id alone.
  const sid = $derived(server.id);
  const onlineNow = $derived(server.players.length);
  let data = $state(null);
  let error = $state('');
  let name = $state('');

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const ops = $derived(new Set((data?.ops || []).map((p) => p.name.toLowerCase())));
  const online = $derived(data?.online || server.players);
  const running = $derived(data ? data.running : server.state === 'running');

  async function load() {
    try {
      data = await api(`${base}/players`);
      error = '';
    } catch (e) {
      // Keep what is on screen when a refresh fails.
      error = e.message;
    }
  }

  /** Runs an action, then reads the lists again shortly after (the server
   * saves its files itself). */
  async function act(action, who, reason = '') {
    error = '';
    await playerAct(server, action, who, reason);
    setTimeout(load, 700);
  }

  async function kickOrBan(action, who) {
    if (await askKickOrBan(server, action, who)) setTimeout(load, 700);
  }

  async function add(action) {
    const n = name.trim();
    if (!n) return;
    name = '';
    if (action === 'ban') await kickOrBan('ban', n);
    else await act(action, n);
  }

  $effect(() => {
    sid;
    onlineNow;
    load();
  });
  $effect(() => {
    sid;
    const t = setInterval(load, 10000);
    return () => clearInterval(t);
  });
</script>

{#snippet face(n)}<img src={head(n, 44)} alt="" width="24" height="24" loading="lazy" />{/snippet}

<div class="col page">
  {#if error}<div class="err">{error}</div>{/if}

  {#if !data && !error}
    <div class="skel"><i></i></div><div class="skel"><i></i></div><div class="skel"><i></i></div>
  {:else}
    <section>
      <h3><Users size={14} />Online now <span class="muted fw">{online.length}</span></h3>
      {#each online as p (p)}
        <div class="pl row-hover">
          {@render face(p)}<span class="nm">{p}</span>
          {#if ops.has(p.toLowerCase())}<span class="chip acc">op</span>{/if}
          <span class="sp"></span>
          <button class="btn sm ghost" onclick={() => act(ops.has(p.toLowerCase()) ? 'deop' : 'op', p)}><Crown size={13} />{ops.has(p.toLowerCase()) ? 'Deop' : 'Op'}</button>
          <button class="btn sm ghost" onclick={() => kickOrBan('kick', p)}><LogOut size={13} />Kick</button>
          <button class="btn sm ghost red" onclick={() => kickOrBan('ban', p)}><Ban size={13} />Ban</button>
        </div>
      {:else}
        <p class="small muted empty">{running ? 'Nobody online.' : server.state === 'starting' ? 'Starting…' : server.state === 'stopping' ? (server.saving ? 'Saving worlds…' : 'Stopping…') : 'The server is stopped.'}</p>
      {/each}
    </section>

    <section>
      <h3><Crown size={14} />Operators</h3>
      {#each data?.ops || [] as p (p.name)}
        <div class="pl row-hover">{@render face(p.name)}<span class="nm">{p.name}</span>
          <span class="small muted">level {p.level ?? 4}</span><span class="sp"></span>
          <button class="btn sm ghost" onclick={() => act('deop', p.name)}>Remove op</button></div>
      {:else}<p class="small muted empty">None.</p>{/each}
    </section>

    <section>
      <h3><ShieldCheck size={14} />Whitelist <span class="chip" class:running={data?.whitelist_on}>{data?.whitelist_on ? 'on' : 'off'}</span></h3>
      {#each data?.whitelist || [] as p (p.name)}
        <div class="pl row-hover">{@render face(p.name)}<span class="nm">{p.name}</span><span class="sp"></span>
          <button class="btn sm ghost" onclick={() => act('whitelist_remove', p.name)}>Remove</button></div>
      {:else}<p class="small muted empty">{data?.whitelist_on ? 'Nobody yet.' : 'Nobody yet. Turn it on in Settings (white-list).'}</p>{/each}
      <form class="row wrap addrow" onsubmit={(e) => { e.preventDefault(); add('whitelist_add'); }}>
        <div class="input namebox"><input bind:value={name} placeholder="Minecraft name" spellcheck="false" autocomplete="off" aria-label="Minecraft name" /></div>
        <button class="btn sm pri" disabled={!name.trim()}><UserPlus size={13} />Add to whitelist</button>
        <button type="button" class="btn sm" disabled={!name.trim()} onclick={() => add('op')}><Crown size={13} />Op</button>
        <button type="button" class="btn sm" disabled={!name.trim()} onclick={() => add('ban')}><Ban size={13} />Ban</button>
      </form>
      {#if !running}<p class="small muted">Start the server to change players: it looks names up and saves its lists itself.</p>{/if}
    </section>

    <section>
      <h3><Ban size={14} />Banned</h3>
      {#each data?.banned || [] as p (p.name)}
        <div class="pl row-hover">{@render face(p.name)}<span class="nm">{p.name}</span>
          <span class="small muted ell" title={p.reason || ''}>{p.reason || ''}</span><span class="sp"></span>
          <button class="btn sm ghost" onclick={() => act('pardon', p.name)}><Undo2 size={13} />Unban</button></div>
      {:else}<p class="small muted empty">Nobody.</p>{/each}
    </section>
  {/if}
</div>

<style>
  .page { gap: 22px; max-width: 760px; padding-bottom: 18px; }
  section { display: flex; flex-direction: column; gap: 2px; }
  h3 { display: flex; align-items: center; gap: 8px; font-size: 13px; font-weight: 600; padding-bottom: 4px; }
  h3 :global(svg) { color: var(--muted); }
  .fw { font-weight: 400; }
  .pl { display: flex; align-items: center; gap: 8px; padding: 4px 8px; border-radius: var(--radius); min-height: 34px; }
  .pl img { width: 24px; height: 24px; border-radius: 4px; background: var(--raise); flex: none; }
  .nm { font-weight: 500; font-size: 13px; }
  .empty { padding: 0 8px; }
  /* Ban is red, as desktop draws it (also on hover). */
  .btn.red, .btn.red:hover:not(:disabled) { color: var(--red); }
  .wrap { flex-wrap: wrap; }
  .addrow { margin-top: 10px; }
  .namebox { width: 16rem; padding: 4px 9px; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; max-width: 260px; }
</style>
