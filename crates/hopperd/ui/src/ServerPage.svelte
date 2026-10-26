<script>
  import { session, outside } from './lib/me.svelte.js';
  import Avatar from './Avatar.svelte';
  import { onMount, onDestroy, tick } from 'svelte';
  import { Play, Square, RotateCw, Skull, CornerDownLeft, Search, ArrowDown, Gauge, Users, Info, Share2, PackagePlus } from '@lucide/svelte';
  import { live, consoles, history, follow, unfollow } from './lib/live.svelte.js';
  import { api, post } from './lib/api.js';
  import { bytes, uptime, color, xmx, stateLabel } from './lib/format.js';
  import { power } from './lib/power.js';
  import { say } from './lib/toast.svelte.js';
  import Spark from './Spark.svelte';
  import Files from './Files.svelte';
  import Editor from './Editor.svelte';
  import Trash from './Trash.svelte';
  import Players from './Players.svelte';
  import Backups from './Backups.svelte';
  import Schedule from './Schedule.svelte';
  import Settings from './Settings.svelte';
  import Mods from './Mods.svelte';
  import Worlds from './Worlds.svelte';
  import Addr from './Addr.svelte';
  import ServerAlerts from './ServerAlerts.svelte';
  import ShareLog from './ShareLog.svelte';
  import PackUpdate from './PackUpdate.svelte';
  import { notePackCheck, shortVersions } from './lib/packs.svelte.js';

  let { id, tab = 'console', path = '' } = $props();

  /** #/server/<id>/<tab>/<path> */
  function nav(t, p = '') {
    const segs = [id, t, ...(p ? p.split('/') : [])].map(encodeURIComponent);
    location.hash = `/server/${segs.join('/')}`;
  }
  const filesTab = $derived(tab === 'files' || tab === 'edit' || tab === 'trash');

  const s = $derived(live.servers.find((x) => x.id === id));
  const lines = $derived(consoles[id] || []);
  const alive = $derived(s && ['running', 'starting', 'stopping'].includes(s.state));
  const cap = $derived(xmx(s?.max_memory));

  let filter = $state('all');
  let query = $state('');
  let cmd = $state('');
  let hist = [];
  let histAt = -1;
  let box = $state();
  let stuck = $state(true);
  let sharing = $state(null);

  function kind(line) {
    if (/\[Hopper\]/.test(line)) return 'h';
    if (/\/(ERROR|FATAL)\]|\bERROR\b|Exception/.test(line)) return 'e';
    if (/\/WARN\]|\bWARN\b/.test(line)) return 'w';
    if (/ (joined|left) the game/.test(line)) return 'j';
    if (/<[^>]+> /.test(line)) return 'c';
    return '';
  }

  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return lines
      .map((text, i) => ({ text, i, k: kind(text) }))
      .filter((l) =>
        (filter === 'all' || (filter === 'e' && l.k === 'e') || (filter === 'w' && (l.k === 'w' || l.k === 'e')) ||
          (filter === 'j' && l.k === 'j') || (filter === 'c' && l.k === 'c')) &&
        (!q || l.text.toLowerCase().includes(q)),
      );
  });
  const counts = $derived({
    e: lines.filter((l) => kind(l) === 'e').length,
    w: lines.filter((l) => kind(l) === 'w').length,
  });

  $effect(() => {
    shown.length;
    if (stuck) tick().then(() => box && (box.scrollTop = box.scrollHeight));
  });

  function onScroll() {
    stuck = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
  }

  function toBottom() {
    stuck = true;
    box.scrollTop = box.scrollHeight;
  }

  async function send(e) {
    e.preventDefault();
    const line = cmd.trim();
    if (!line) return;
    try {
      await post(`/servers/${encodeURIComponent(id)}/command`, { line });
      hist = [line, ...hist.filter((h) => h !== line)].slice(0, 50);
      histAt = -1;
      cmd = '';
      toBottom();
    } catch (err) {
      say(err.message, true);
    }
  }

  function keys(e) {
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

  // The server's modpack and newer versions of it (cached by the panel).
  let packCheck = $state(null);
  /** The check the update dialog opened with (it keeps it while the check refreshes). */
  let packOpen = $state(null);
  const packNewer = $derived(packCheck?.pack ? packCheck.newer || [] : []);
  const packShort = $derived(packNewer.length ? shortVersions(packCheck.pack.current.name, packNewer[0].name) : null);

  async function checkPack(force = false) {
    try {
      packCheck = await api(`/servers/${encodeURIComponent(id)}/pack/check${force ? '?force=1' : ''}`);
      notePackCheck(id, packCheck);
    } catch {
      // no CurseForge key, offline, or not a pack: no notice
    }
  }

  onMount(() => {
    follow(id);
    checkPack();
  });
  onDestroy(() => unfollow(id));
</script>

{#if !s}
  <div class="pad muted">{live.servers.length ? 'No server with that id.' : 'Loading…'}</div>
{:else}
  <div class="head">
    <Avatar server={s} cls="ic big" />
    <div class="ttl">
      <div class="row wrap">
        <h2>{s.name}</h2>
        <span class="chip {s.state}"><span class="dot {s.state}"></span>{stateLabel[s.state]}{alive ? ` · ${uptime(s.uptime_secs)}` : ''}{s.exit_code != null ? ` · exit ${s.exit_code}` : ''}</span>
        {#if s.managed_by}<span class="chip" title="{s.managed_by} also has this folder. It runs here in place; don't start it in both at once.">Also in {s.managed_by}</span>{/if}{#if outside(s.path)}<span class="chip" title="It runs where it is, outside the panel folder ({session.home})">Outside the panel folder</span>{/if}<ServerAlerts server={s} part="chip" />
        {#if packShort}<button class="chip acc packup" title="{packCheck.pack.name}: {packNewer.length} newer version{packNewer.length === 1 ? '' : 's'}. Review the changelog and update." onclick={() => (packOpen = packCheck)}><PackagePlus size={12} />Pack update {packShort[0]} → {packShort[1]}</button>{/if}
      </div>
      <div class="small muted ell">{s.loader || 'Minecraft'}{s.version ? ` · Minecraft ${s.version}` : ''} · {#if s.addresses?.length}<Addr address={s.addresses[0]} more={s.addresses.length - 1} />{/if}<span class="mono" class:gap={s.addresses?.length}>:{s.port}</span>{#if s.sleep_after_minutes && s.state === 'stopped'}<span class="sleep gap">· Sleeping, joining wakes it</span>{/if} · <span class="mono">{s.path}</span></div>
    </div>
    <div class="row acts">
      {#if !alive}
        <button class="btn go" onclick={() => power(s, 'start')}><Play size={14} />Start</button>
      {:else}
        <button class="btn" onclick={() => power(s, 'restart')}><RotateCw size={14} />Restart</button>
        <button class="btn bad" disabled={s.state === 'stopping'} onclick={() => power(s, 'stop')}><Square size={14} />Stop</button>
        <button class="btn icon bad" title="Kill (no save)" onclick={() => power(s, 'kill')}><Skull size={14} /></button>
      {/if}
    </div>
  </div>

  <ServerAlerts server={s} part="banner" />

  <nav class="tabs">
    <button class:on={tab === 'console'} onclick={() => nav('console')}>Console</button>
    <button class:on={filesTab} onclick={() => nav('files')}>Files</button>
    <button class:on={tab === 'mods'} onclick={() => nav('mods')}>Mods</button>
    <button class:on={tab === 'worlds'} onclick={() => nav('worlds')}>Worlds</button>
    <button class:on={tab === 'players'} onclick={() => nav('players')}>Players</button>
    <button class:on={tab === 'backups'} onclick={() => nav('backups')}>Backups</button>
    <button class:on={tab === 'schedule'} onclick={() => nav('schedule')}>Schedule</button>
    <button class:on={tab === 'settings'} onclick={() => nav('settings')}>Settings</button>
  </nav>

  {#if tab === 'files'}
    <div class="full"><Files server={s} {path} go={(p) => nav('files', p)} openFile={(p) => nav('edit', p)} openTrash={() => nav('trash')} /></div>
  {:else if tab === 'edit'}
    <div class="full">{#key path}<Editor server={s} {path} close={() => nav('files', path.split('/').slice(0, -1).join('/'))} />{/key}</div>
  {:else if tab === 'trash'}
    <div class="full"><Trash server={s} close={() => nav('files')} /></div>
  {:else if tab === 'mods'}
    <div class="full scroll"><Mods server={s} /></div>
  {:else if tab === 'worlds'}
    <div class="full scroll"><Worlds server={s} openFiles={(p) => nav('files', p)} openBackups={() => nav('backups')} /></div>
  {:else if tab === 'players'}
    <div class="full scroll"><Players server={s} /></div>
  {:else if tab === 'backups'}
    <div class="full scroll"><Backups server={s} openSchedule={() => nav('schedule')} /></div>
  {:else if tab === 'schedule'}
    <div class="full scroll"><Schedule server={s} /></div>
  {:else if tab === 'settings'}
    <div class="full scroll"><Settings server={s} /></div>
  {:else}
  <div class="split">
    <section class="term">
      <div class="tb">
        <button class="chip" class:on={filter === 'all'} onclick={() => (filter = 'all')}>All</button>
        <button class="chip" class:on={filter === 'e'} onclick={() => (filter = 'e')}>Errors {counts.e}</button>
        <button class="chip" class:on={filter === 'w'} onclick={() => (filter = 'w')}>Warnings {counts.w}</button>
        <button class="chip" class:on={filter === 'j'} onclick={() => (filter = 'j')}>Joins</button>
        <button class="chip" class:on={filter === 'c'} onclick={() => (filter = 'c')}>Chat</button>
        <span class="sp"></span>
        <div class="input find"><Search size={13} /><input placeholder="Filter" bind:value={query} /></div>
        <button class="btn sm" title="Share a log or crash report on mclo.gs" onclick={() => (sharing = { path: '' })}><Share2 size={13} />Share log</button>
      </div>
      <div class="lines" bind:this={box} onscroll={onScroll}>
        {#each shown as l (l.i)}
          <div class={l.k}>{l.text}</div>
        {:else}
          <div class="muted">{lines.length ? 'Nothing matches.' : alive ? 'Waiting for output…' : 'The server is stopped. Its console shows here when it runs.'}</div>
        {/each}
      </div>
      {#if !stuck}
        <button class="btn sm jump" onclick={toBottom}><ArrowDown size={13} />Latest</button>
      {/if}
      <form class="cmd" onsubmit={send}>
        <span class="mono prompt">&gt;</span>
        <input bind:value={cmd} onkeydown={keys} placeholder={alive ? 'Command (↑ for history)' : 'Start the server to send commands'} disabled={!alive} spellcheck="false" autocomplete="off" />
        <button class="btn sm pri" disabled={!alive || !cmd.trim()}><CornerDownLeft size={13} /></button>
      </form>
    </section>

    <aside class="rail">
      <div class="card">
        <h3><Gauge size={14} />Live</h3>
        <div class="t">CPU<b>{Math.round(s.cpu)} %</b></div>
        <Spark values={history[`${id}.cpu`]} color="#f97316" />
        <div class="t gap">RAM<b>{bytes(s.mem_bytes)}{cap ? ` / ${bytes(cap)}` : ''}</b></div>
        <Spark values={history[`${id}.mem`]} color="#60a5fa" />
        <ServerAlerts server={s} part="live" />
      </div>
      <div class="card">
        <h3><Users size={14} />Players <span class="muted fw">{s.players.length} / {s.max_players}</span></h3>
        {#each s.players as p (p)}
          <div class="pl"><img src="https://mc-heads.net/avatar/{encodeURIComponent(p)}/20" alt="" width="20" height="20" loading="lazy" />{p}</div>
        {:else}
          <div class="small muted">Nobody online.</div>
        {/each}
      </div>
      <div class="card">
        <h3><Info size={14} />Server</h3>
        <div class="kv">
          <span>Port</span><span class="mono">{s.port}</span>
          <span>Memory</span><span>{s.max_memory ? `-Xmx${s.max_memory}` : 'not set'}</span>
          <span>Loader</span><span>{s.loader || '—'}</span>
          <span>Version</span><span>{s.version || '—'}</span>
        </div>
      </div>
    </aside>
  </div>
  {/if}
{/if}


{#if sharing && s}<ShareLog server={s} path={sharing.path} onClose={() => (sharing = null)} />{/if}
{#if packOpen && s}<PackUpdate server={s} check={packOpen} onClose={() => (packOpen = null)} onDone={() => checkPack(true)} openFiles={(p) => nav('files', p)} />{/if}
<style>
  .pad { padding: 18px; }
  .head { display: flex; align-items: center; gap: 12px; padding: 16px 18px 12px; flex-wrap: wrap; }
  .ttl { flex: 1; min-width: 220px; }
  .wrap { flex-wrap: wrap; }
  h2 { font-size: 17px; font-weight: 700; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sleep { color: var(--gold); }
  .packup { cursor: pointer; max-width: 320px; overflow: hidden; text-overflow: ellipsis; }
  .packup:hover { filter: brightness(1.2); }
  .gap { margin-left: 6px; }
  .tabs { display: flex; gap: 20px; padding: 0 18px; border-bottom: 1px solid var(--line); margin-bottom: 14px; }
  .tabs button { padding: 9px 0; color: var(--subtle); font-weight: 500; }
  .tabs button.on { color: var(--text); box-shadow: inset 0 -2px var(--acc); }
  .full { flex: 1; min-height: 0; display: flex; flex-direction: column; padding: 0 18px 18px; }
  .full.scroll { overflow: auto; display: block; }
  .tabs { overflow-x: auto; }
  .split { flex: 1; min-height: 0; display: flex; gap: 14px; padding: 0 18px 18px; }
  .term { flex: 1; min-width: 0; min-height: 360px; background: var(--term); border: 1px solid var(--line); border-radius: 10px; display: flex; flex-direction: column; overflow: hidden; position: relative; }
  .tb { display: flex; align-items: center; gap: 6px; padding: 8px 10px; border-bottom: 1px solid var(--line); background: var(--base); flex-wrap: wrap; }
  .find { padding: 3px 8px; width: 180px; }
  .lines { flex: 1; overflow: auto; padding: 10px 12px; font: 12px/1.6 var(--mono); color: #c9c9d1; }
  .lines div { white-space: pre-wrap; word-break: break-word; }
  .lines .e { color: var(--red); } .lines .w { color: var(--gold); } .lines .j { color: var(--green); }
  .lines .h { color: var(--acc); } .lines .c { color: #dcdce4; }
  .jump { position: absolute; right: 16px; bottom: 56px; box-shadow: 0 6px 20px #0008; }
  .cmd { display: flex; align-items: center; gap: 8px; border-top: 1px solid var(--line); padding: 8px 10px; background: var(--base); }
  .cmd input { flex: 1; background: none; border: 0; outline: 0; font: 12.5px var(--mono); }
  .prompt { color: var(--acc); }
  .rail { width: 290px; flex: none; display: flex; flex-direction: column; gap: 12px; overflow: auto; }
  .t { display: flex; justify-content: space-between; font-size: 11.5px; color: var(--muted); }
  .t b { color: var(--text); font-weight: 500; }
  .gap { margin-top: 10px; }
  .fw { font-weight: 400; }
  .pl { display: flex; align-items: center; gap: 8px; padding: 5px 0; border-bottom: 1px solid var(--line); }
  .pl:last-child { border: 0; }
  .pl img { border-radius: 4px; background: var(--raise); }
  .kv { display: grid; grid-template-columns: auto 1fr; gap: 6px 14px; font-size: 12.5px; }
  .kv span:nth-child(odd) { color: var(--muted); }
  @media (max-width: 900px) { .split { flex-direction: column; } .rail { width: auto; } .term { min-height: 60vh; } }
</style>
