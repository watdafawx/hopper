<script>
  import { session, outside } from './lib/me.svelte.js';
  import Avatar from './Avatar.svelte';
  import { Play, Square, RotateCw, Terminal, Plus, Server } from '@lucide/svelte';
  import { live, history } from './lib/live.svelte.js';
  import { bytes, uptime, xmx, stateText } from './lib/format.js';
  import { power } from './lib/power.js';
  import { packUpdates } from './lib/packs.svelte.js';
  import Spark from './Spark.svelte';
  import Addr from './Addr.svelte';
  import ServerAlerts from './ServerAlerts.svelte';

  let { onNew = () => {} } = $props();

  // Alerts about the PC itself (drive nearly full, memory): the server
  // pages show them too, but this is where they belong.
  const PANEL = { id: '', state: '' };

  const h = $derived(live.host);
  const players = $derived(live.servers.reduce((n, s) => n + s.players.length, 0));
  const serverMem = $derived(live.servers.reduce((n, s) => n + s.mem_bytes, 0));
  const alive = (s) => s.state === 'running' || s.state === 'starting' || s.state === 'stopping';
  // Running servers first, as in the server list.
  const servers = $derived([...live.servers].sort((a, b) => Number(alive(b)) - Number(alive(a))));
  const loading = $derived(!live.connected && !live.servers.length);
  const href = (s) => `#/server/${encodeURIComponent(s.id)}`;
</script>

<div class="pad">
  <div class="row hd"><h2>Overview</h2><span class="sp"></span>
    <button class="btn pri" onclick={onNew}><Plus size={14} />New server</button></div>
  <div class="alerts"><ServerAlerts server={PANEL} part="banner" /></div>
  <div class="grid4">
    <div class="card stat">
      <div class="lbl">CPU{h?.cpu_name ? ` · ${h.cpu_name.replace(/\(R\)|\(TM\)|CPU|Processor|\d+-Core/g, '').trim()}` : ''}</div>
      <div class="v">{h ? Math.round(h.cpu) : '–'} <span class="u">%</span></div>
      <div class="s">{h?.threads ?? '–'} threads</div>
      <Spark values={history['host.cpu']} color="#f97316" />
    </div>
    <div class="card stat">
      <div class="lbl">Memory</div>
      <div class="v">{h ? bytes(h.mem_used) : '–'} <span class="u">/ {h ? bytes(h.mem_total) : '–'}</span></div>
      <div class="s">{bytes(serverMem)} to servers</div>
      <Spark values={history['host.mem']} color="#38bdf8" />
    </div>
    <div class="card stat">
      <div class="lbl">Disk{h?.disk_mount ? ` · ${h.disk_mount}` : ''}</div>
      <div class="v">{h?.disk_total ? bytes(h.disk_total - h.disk_free) : '–'} <span class="u">/ {h?.disk_total ? bytes(h.disk_total) : '–'}</span></div>
      <div class="s">{h?.disk_total ? `${bytes(h.disk_free)} free` : 'Panel folder not set'}</div>
      <div class="bar g dbar"><i style:width="{h?.disk_total ? ((h.disk_total - h.disk_free) / h.disk_total) * 100 : 0}%"></i></div>
    </div>
    <div class="card stat">
      <div class="lbl">Players online</div>
      <div class="v">{players}</div>
      <div class="s">{live.servers.filter(alive).length} of {live.servers.length} servers up · host up {uptime(h?.uptime_secs)}</div>
    </div>
  </div>

  <div class="servers">
    {#if loading}
      {#each [0, 1] as i (i)}<div class="scard"><div class="skel"><i></i></div><div class="skel"><i></i></div></div>{/each}
    {/if}
    {#each servers as s (s.id)}
      {@const cap = xmx(s.max_memory)}
      {@const up = packUpdates.byId[s.id]}
      <div class="scard">
        <div class="row">
          <a class="icl" href={href(s)}><Avatar server={s} cls="ic" /></a>
          <a class="nmw" href={href(s)}>
            <div class="nm">{s.name}</div>
            <div class="tiny muted cap">
              {s.loader || 'Minecraft'}{s.version ? ` ${s.version}` : ''} · {#if s.addresses?.length}<Addr address={s.addresses[0]} more={s.addresses.length - 1} />{:else}:{s.port}{/if}{alive(s) ? ` · up ${uptime(s.uptime_secs)}` : ''}{#if s.sleep_after_minutes && s.state === 'stopped'} · Sleeping{/if}
            </div>
          </a>
          {#if s.state === 'running'}
            <span class="pill running" title="Players online"><span class="glow"></span>{s.players.length}/{s.max_players}</span>
          {:else if s.state === 'stopped'}
            <span class="stopped"><span class="dot"></span>Stopped</span>
          {:else}
            <span class="pill {s.state}">{stateText(s)}{s.exit_code != null ? ` (${s.exit_code})` : ''}</span>
          {/if}
        </div>
        {#if up || s.managed_by || outside(s.path)}
          <div class="row chips">
            {#if up}<a class="chip acc" href={href(s)} title="Pack update: {up.from} → {up.to}">Pack update · {up.to}</a>{/if}
            {#if s.managed_by}<span class="chip" title="{s.managed_by} also has this folder">Also in {s.managed_by}</span>{/if}
            {#if outside(s.path)}<span class="chip" title="It runs where it is, outside the panel folder ({session.home})">Outside the panel folder</span>{/if}
          </div>
        {/if}
        <div class="meters">
          <div>
            <div class="t">CPU<b>{Math.round(s.cpu)} %</b></div>
            <div class="bar"><i style:width="{Math.min(100, s.cpu)}%"></i></div>
          </div>
          <div>
            <div class="t">RAM<b>{bytes(s.mem_bytes)}{cap ? ` / ${bytes(cap)}` : ''}</b></div>
            <div class="bar b"><i style:width="{cap ? Math.min(100, (s.mem_bytes / cap) * 100) : 0}%"></i></div>
          </div>
          <div>
            <div class="t">Players<b>{s.players.length} / {s.max_players}</b></div>
            <div class="bar g"><i style:width="{(s.players.length / Math.max(1, s.max_players)) * 100}%"></i></div>
          </div>
        </div>
        <div class="row">
          <span class="small subtle ell">{s.players.join(', ') || (alive(s) ? 'Nobody online' : '')}</span>
          <span class="sp"></span>
          {#if !alive(s)}
            <button class="btn sm go" onclick={() => power(s, 'start')}><Play size={13} />Start</button>
          {:else}
            <button class="btn sm icon" title="Restart" onclick={() => power(s, 'restart')}><RotateCw size={13} /></button>
            <button class="btn sm icon bad" title="Stop" onclick={() => power(s, 'stop')}><Square size={13} /></button>
          {/if}
          <a class="btn sm" href={href(s)}><Terminal size={13} />Console</a>
        </div>
      </div>
    {:else}
      {#if !loading}
        <div class="card empty">
          <div class="empty-state">
            <span class="es-ic"><Server size={18} /></span>
            <b>No servers yet</b>
            <p>Make one with New server, from a modpack or a server pack zip. Every folder with a <span class="mono">server.properties</span> in the panel's servers folder shows here too.</p>
            <button class="btn sm pri" onclick={onNew}><Plus size={13} />New server</button>
          </div>
        </div>
      {/if}
    {/each}
  </div>
</div>

<style>
  .alerts :global(.ban) { margin: 0 0 10px; }
  .icl { display: flex; flex: none; }
  .pad { padding: 18px; }
  h2 { font-size: 17px; font-weight: 600; }
  .hd { margin-bottom: 14px; }
  .grid4 { display: grid; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); gap: 12px; }
  .stat .v { font-size: 22px; font-weight: 700; margin-top: 4px; white-space: nowrap; }
  .stat .u { font-size: 12px; color: var(--muted); font-weight: 400; }
  .stat .s { font-size: 12px; color: var(--muted); }
  .dbar { margin-top: 16px; }
  .servers { display: grid; grid-template-columns: repeat(auto-fill, minmax(330px, 1fr)); gap: 12px; margin-top: 12px; }
  .scard { background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius-lg); padding: 14px; display: flex; flex-direction: column; gap: 12px; }
  .scard:hover { border-color: var(--line2); }
  .nmw { flex: 1; min-width: 0; color: inherit; }
  .nmw:hover .nm { color: var(--text); text-decoration: underline; text-decoration-color: var(--line2); }
  .nm { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text); }
  .cap { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chips { flex-wrap: wrap; gap: 6px; margin-top: -4px; }
  .stopped { display: inline-flex; align-items: center; gap: 6px; font-size: 11px; color: var(--muted); flex: none; }
  .meters { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px; }
  .t { display: flex; justify-content: space-between; font-size: 11.5px; color: var(--muted); margin-bottom: 4px; }
  .t b { color: var(--text); font-weight: 500; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .empty { grid-column: 1 / -1; padding: 0; }
  @media (max-width: 760px) { .pad { padding: 16px; } .servers { grid-template-columns: 1fr; } }
</style>
