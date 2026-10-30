<script>
  // The mod remove question and its Undo, for every Remove on this page (Mods, the crash card, the editor's mod panel).
  import ModsRemove from './ModsRemove.svelte';
  import { session, outside } from './lib/me.svelte.js';
  import Avatar from './Avatar.svelte';
  import { onMount, onDestroy } from 'svelte';
  import { Play, Square, RotateCw, Ellipsis, Archive, Skull, Gauge, Users, Info, PackagePlus, SquareTerminal, FolderOpen, Puzzle, Globe, Clock, Settings2, LogOut, Crown, UserPlus, UserMinus, ShieldOff, ChevronDown } from '@lucide/svelte';
  import { live, history, follow, unfollow } from './lib/live.svelte.js';
  import { api, post } from './lib/api.js';
  import { bytes, uptime, xmx, stateText, memTitle, loaderText } from './lib/format.js';
  import { say } from './lib/toast.svelte.js';
  import { ask } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { askPower, askKickOrBan, playerAct, head } from './lib/consoleKit.js';
  import ConsoleView from './ConsoleView.svelte';
  import ServerCrash from './ServerCrash.svelte';
  import ModPeek from './ModPeek.svelte';
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
  import PackVersions from './PackVersions.svelte';
  import LoaderVersions from './LoaderVersions.svelte';
  import { notePackCheck, shortVersions } from './lib/packs.svelte.js';

  let { id, tab = 'console', path = '' } = $props();

  /** #/server/<id>/<tab>/<path> */
  function nav(t, p = '') {
    const segs = [id, t, ...(p ? p.split('/') : [])].map(encodeURIComponent);
    location.hash = `/server/${segs.join('/')}`;
  }
  const filesTab = $derived(tab === 'files' || tab === 'edit' || tab === 'trash');

  const s = $derived(live.servers.find((x) => x.id === id));
  const alive = $derived(s && ['running', 'starting', 'stopping'].includes(s.state));
  const cap = $derived(xmx(s?.max_memory));
  const online = $derived(s?.state === 'running' ? s.players.length : null);

  let sharing = $state(null);
  /** A mod the crash card opened, shown beside the console. */
  let peek = $state(null);
  /** The header's "…" menu, and a player's menu in the Players card. */
  let more = $state(false);
  let playerMenu = $state(null);

  // The tabs, in desktop's order for a Hopper Server's servers.
  const TABS = [
    ['console', 'Console', SquareTerminal],
    ['files', 'Files', FolderOpen],
    ['mods', 'Mods', Puzzle],
    ['worlds', 'Worlds', Globe],
    ['players', 'Players', Users],
    ['backups', 'Backups', Archive],
    ['schedule', 'Schedule', Clock],
    ['settings', 'Settings', Settings2],
  ];
  const onTab = (t) => (t === 'files' ? filesTab : tab === t);

  async function backUpNow() {
    more = false;
    const note = await ask('Back up everything', '', {
      body: alive ? 'The world is saved first and saving is paused while the backup is made.' : 'A note to find it by (optional).',
      ok: 'Back up',
      placeholder: 'Before the 1.21 update',
    });
    if (note === null) return;
    try {
      const { job } = await post(`/servers/${encodeURIComponent(id)}/backups`, { worlds_only: false, note });
      say(`Backing up ${s.name}.`);
      const b = await followJob(job);
      say(`Backed up: ${b?.added ? `${bytes(b.added)} new` : 'nothing changed since the last one'}${b?.files != null ? ` · ${b.files} files` : ''}`);
    } catch (e) {
      say(e.message, true);
    }
  }

  function kill() {
    more = false;
    askPower(s, 'kill');
  }

  function openPlayer(e, p) {
    e.stopPropagation();
    const r = e.currentTarget.getBoundingClientRect();
    playerMenu = playerMenu?.name === p ? null : { name: p, x: Math.min(r.left, innerWidth - 220), y: r.bottom + 4 };
  }

  async function playerDo(action) {
    const who = playerMenu.name;
    playerMenu = null;
    const DONE = {
      op: `${who} is an operator now.`,
      deop: `${who} is no longer an operator.`,
      whitelist_add: `Added ${who} to the whitelist.`,
      whitelist_remove: `Removed ${who} from the whitelist.`,
    };
    if (action === 'kick') {
      if (await askKickOrBan(s, 'kick', who)) say(`Kicked ${who}.`);
    } else if (await playerAct(s, action, who)) {
      say(DONE[action]);
    }
  }

  // The server's modpack and newer versions of it (cached by the panel).
  let packCheck = $state(null);
  /** The check the update dialog opened with (it keeps it while the check refreshes). */
  let packOpen = $state(null);
  /** The pack version the update dialog updates to (null: the newest). */
  let packFile = $state(null);
  // Clicking the pack or loader version in the header, as in desktop.
  let versionsOpen = $state(null);
  let loaderOpen = $state(false);
  function openUpdate(file = null) {
    versionsOpen = null;
    packFile = file;
    packOpen = packCheck;
  }
  function openVersions() {
    packOpen = null;
    versionsOpen = packCheck;
  }
  // A mod loader the panel can change (not vanilla, Paper's own builds…).
  const hasLoader = $derived(!!s?.loader && /^(neoforge|forge|fabric|quilt)/i.test(s.loader));
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
        <span class="chip {s.state}"><span class="dot {s.state}"></span>{stateText(s)}{alive ? ` · ${uptime(s.uptime_secs)}` : ''}{s.exit_code != null ? ` · exit ${s.exit_code}` : ''}</span>
        {#if s.managed_by}<span class="chip" title="{s.managed_by} also has this folder. It runs here in place; don't start it in both at once.">Also in {s.managed_by}</span>{/if}{#if outside(s.path)}<span class="chip" title="It runs where it is, outside the panel folder ({session.home})">Outside the panel folder</span>{/if}<ServerAlerts server={s} part="chip" />
        {#if packShort}<button class="chip acc packup" title="{packCheck.pack.name}: {packNewer.length} newer version{packNewer.length === 1 ? '' : 's'}. Review the changelog and update." onclick={() => openUpdate()}><PackagePlus size={12} />Pack update {packShort[0]} → {packShort[1]}</button>{/if}
      </div>
      <div class="small muted ell">{#if packCheck?.pack}<button class="vlink" title="{packCheck.pack.name}: versions and changelogs" onclick={openVersions}>{packCheck.pack.current.name}</button>{' · '}{/if}{#if s.version}<button class="vlink" title="Change Minecraft version (Settings › Version)" onclick={() => nav('settings', 'version')}>Minecraft {s.version}</button>{:else}Minecraft{/if}{#if s.loader}{' · '}{#if hasLoader}<button class="vlink" title="Loader versions and changelog" onclick={() => (loaderOpen = true)}>{loaderText(s.loader)}</button>{:else}{loaderText(s.loader)}{/if}{/if} · {#if s.addresses?.length}<Addr address={s.addresses[0]} more={s.addresses.length - 1} />{/if}<span class="mono" class:gl={s.addresses?.length}>:{s.port}</span>{#if s.sleep_after_minutes && s.state === 'stopped'}<span class="sleep gl">· Sleeping, joining wakes it</span>{/if} · <span class="mono">{s.path}</span></div>
    </div>
    <div class="row acts">
      {#if !alive}
        <button class="btn go" onclick={() => askPower(s, 'start')}><Play size={14} />Start</button>
      {:else}
        <button class="btn" disabled={s.state === 'stopping'} onclick={() => askPower(s, 'stop')}><Square size={14} />Stop</button>
        <button class="btn" disabled={s.state === 'stopping'} onclick={() => askPower(s, 'restart')}><RotateCw size={14} />Restart</button>
      {/if}
      <div class="morewrap">
        <button class="btn icon ghost" title="More" aria-haspopup="menu" aria-expanded={more} onclick={(e) => { e.stopPropagation(); playerMenu = null; more = !more; }}><Ellipsis size={15} /></button>
        {#if more}
          <div class="popover menu drop" role="menu">
            <button class="menu-item" role="menuitem" onclick={backUpNow}><Archive size={13} />Back up now</button>
            <div class="menu-sep"></div>
            <button class="menu-item bad" role="menuitem" disabled={!alive} onclick={kill}><Skull size={13} />Kill (no save)</button>
          </div>
        {/if}
      </div>
    </div>
  </div>

  <ServerAlerts server={s} part="banner" />

  <nav class="tabs">
    {#each TABS as [t, label, Icon] (t)}
      <button class:tab-sel={onTab(t)} onclick={() => nav(t)}><Icon size={14} />{label}{t === 'players' && online != null ? ` ${online}` : ''}</button>
    {/each}
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
    <div class="full scroll">{#key path}<Settings server={s} start={path} />{/key}</div>
  {:else}
  <div class="split">
    <div class="main">
      <ServerCrash server={s} onPeek={(m) => (peek = m)} onShare={(p) => (sharing = { path: p })} openFile={(p) => nav('edit', p)} />
      <ConsoleView server={s} onShare={(p) => (sharing = { path: p })} />
    </div>

    {#if peek}
      <ModPeek server={s} mod={peek} onClose={() => (peek = null)} onChanged={() => {}} onUpdate={() => { peek = null; nav('mods'); }} />
    {:else}
    <aside class="rail">
      <div class="card">
        <h3><Gauge size={14} />Live</h3>
        <!-- As desktop's Live cards: a dash and the state while it isn't running. -->
        <div class="t">CPU<b>{alive ? `${Math.round(s.cpu)} %` : '—'}</b></div>
        {#if alive}<Spark values={history[`${id}.cpu`]} color="var(--acc)" />{/if}
        <div class="t gap" title={alive ? memTitle(s.mem_bytes, cap) : undefined}>RAM<b>{alive ? `${bytes(s.mem_bytes)}${cap ? ` / ${bytes(cap)}` : ''}` : cap ? `— / ${bytes(cap)}` : '—'}</b></div>
        {#if alive}<Spark values={history[`${id}.mem`]} color="var(--blue)" />{:else}<div class="tiny muted gap">{stateText(s)}</div>{/if}
        <ServerAlerts server={s} part="live" />
      </div>
      <div class="card">
        <h3><Users size={14} />Players online <span class="muted fw">{s.players.length} / {s.max_players}</span></h3>
        {#each s.players as p (p)}
          <button class="pl row-hover" aria-haspopup="menu" title="Kick, operator, whitelist" onclick={(e) => openPlayer(e, p)}><img src={head(p, 20)} alt="" width="20" height="20" loading="lazy" /><span class="grow">{p}</span><ChevronDown size={12} /></button>
        {:else}
          <div class="small muted">{s.state === 'running' ? 'Nobody online.' : s.state === 'starting' ? 'Starting…' : s.state === 'stopping' ? (s.saving ? 'Saving worlds…' : 'Stopping…') : 'The server is stopped.'}</div>
        {/each}
      </div>
      <div class="card">
        <h3><Info size={14} />Server</h3>
        <div class="kv">
          <span>Port</span><span class="mono">{s.port}</span>
          <span>Memory</span><span>{s.max_memory ? `-Xmx${s.max_memory}` : 'not set'}</span>
          <span>Loader</span><span>{loaderText(s.loader) || '—'}</span>
          <span>Version</span><span>{s.version || '—'}</span>
        </div>
      </div>
    </aside>
    {/if}
  </div>
  {/if}
{/if}

{#if playerMenu && s}
  <div class="popover menu pmenu" style="left: {playerMenu.x}px; top: {playerMenu.y}px" role="menu">
    <button class="menu-item" role="menuitem" onclick={() => playerDo('kick')}><LogOut size={13} />Kick…</button>
    <button class="menu-item" role="menuitem" onclick={() => playerDo('op')}><Crown size={13} />Make operator</button>
    <button class="menu-item" role="menuitem" onclick={() => playerDo('deop')}><ShieldOff size={13} />Remove operator</button>
    <div class="menu-sep"></div>
    <button class="menu-item" role="menuitem" onclick={() => playerDo('whitelist_add')}><UserPlus size={13} />Add to whitelist</button>
    <button class="menu-item" role="menuitem" onclick={() => playerDo('whitelist_remove')}><UserMinus size={13} />Remove from whitelist</button>
  </div>
{/if}

<svelte:window onclick={() => { more = false; playerMenu = null; }} onkeydown={(e) => { if (e.key === 'Escape') { more = false; playerMenu = null; } }} />


{#if sharing && s}<ShareLog server={s} path={sharing.path} onClose={() => (sharing = null)} />{/if}
{#if packOpen && s}{#key packFile}<PackUpdate server={s} check={packOpen} file={packFile} onClose={() => (packOpen = null)} onDone={() => checkPack(true)} openFiles={(p) => nav('files', p)} onOtherVersion={openVersions} />{/key}{/if}
{#if versionsOpen && s}<PackVersions server={s} check={versionsOpen} onPick={openUpdate} onClose={() => (versionsOpen = null)} />{/if}
{#if loaderOpen && s}<LoaderVersions server={s} onClose={() => (loaderOpen = false)} />{/if}
<ModsRemove />
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
  .gl { margin-left: 6px; }
  .vlink { color: inherit; border-bottom: 1px dashed #ffffff30; padding: 0; }
  .vlink:hover { color: var(--acc); border-color: var(--acc); }
  .tabs { display: flex; gap: 20px; padding: 0 18px; border-bottom: 1px solid var(--line); margin-bottom: 14px; overflow-x: auto; }
  .tabs button { display: inline-flex; align-items: center; gap: 6px; padding: 9px 0; color: var(--subtle); font-weight: 500; white-space: nowrap; }
  .tabs button:hover { color: var(--text); }
  .tabs button :global(svg) { color: var(--muted); }
  .tabs button.tab-sel :global(svg) { color: var(--acc); }
  .morewrap { position: relative; }
  .drop { position: absolute; right: 0; top: calc(100% + 4px); z-index: 40; }
  .menu-item.bad { color: var(--red); }
  .menu-item:disabled { opacity: .45; cursor: default; background: none; }
  .pmenu { position: fixed; z-index: 80; }
  .full { flex: 1; min-height: 0; display: flex; flex-direction: column; padding: 0 18px 18px; }
  .full.scroll { overflow: auto; display: block; }
  .split { flex: 1; min-height: 0; display: flex; gap: 14px; padding: 0 18px 18px; }
  .main { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .rail { width: 290px; flex: none; display: flex; flex-direction: column; gap: 12px; overflow: auto; }
  .t { display: flex; justify-content: space-between; font-size: 11.5px; color: var(--muted); }
  .t b { color: var(--text); font-weight: 500; }
  .gap { margin-top: 10px; }
  .fw { font-weight: 400; }
  .pl { display: flex; align-items: center; gap: 8px; padding: 4px 6px; margin: 0 -6px; width: calc(100% + 12px); border-radius: var(--radius); text-align: left; color: var(--text); }
  .pl :global(svg) { color: var(--muted); flex: none; }
  .pl img { border-radius: 4px; background: var(--raise); flex: none; }
  .grow { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .kv { display: grid; grid-template-columns: auto 1fr; gap: 6px 14px; font-size: 12.5px; }
  .kv span:nth-child(odd) { color: var(--muted); }
  @media (max-width: 900px) { .split { flex-direction: column; } .rail { width: auto; } .main :global(.term) { min-height: 60vh; } }
</style>
