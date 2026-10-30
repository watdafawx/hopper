<script>
  import { session, outside } from './lib/me.svelte.js';
  import Avatar from './Avatar.svelte';
  import { Play, Square, RotateCw, Terminal, Plus, Server } from '@lucide/svelte';
  import { live, history } from './lib/live.svelte.js';
  import { bytes, uptime, xmx, stateText, bootText, memTitle, loaderText } from './lib/format.js';
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

{#snippet chips(s, up)}
  {#if up}<a class="chip acc" href={href(s)} title="Pack update: {up.from} → {up.to}">Pack update · {up.to}</a>{/if}
  {#if s.managed_by}<span class="chip" title="{s.managed_by} also has this folder">Also in {s.managed_by}</span>{/if}
  {#if outside(s.path)}<span class="chip" title="It runs where it is, outside the panel folder ({session.home})">Outside the panel folder</span>{/if}
{/snippet}

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
      {@const on = alive(s)}
      {@const mem = on ? s.mem_bytes : 0}
      <!-- The bar spans the larger of use and setting, so going over shows as a hatched tail past a tick at the setting. -->
      {@const span = Math.max(mem, cap)}
      {@const nchips = Number(!!up) + Number(!!s.managed_by) + Number(outside(s.path))}
      <div class="scard">
        <div class="row">
          <a class="icl" href={href(s)}><Avatar server={s} cls="ic" /></a>
          <a class="nmw" href={href(s)}>
            <div class="nm">{s.name}</div>
            <div class="tiny muted cap" title={[loaderText(s.loader), s.version].filter(Boolean).join(' · ')}>
              {loaderText(s.loader, true) || 'Minecraft'}{s.version ? ` ${s.version}` : ''} · {#if s.addresses?.length}<Addr address={s.addresses[0]} more={s.addresses.length - 1} />{:else}:{s.port}{/if}{on ? ` · up ${uptime(s.uptime_secs)}` : ''}{#if s.sleep_after_minutes && s.state === 'stopped'} · Sleeping{/if}
            </div>
          </a>
          {#if s.state === 'running'}
            <ServerAlerts server={s} part="chip" />
            <span class="pill running" title="Players online"><span class="glow"></span>{s.players.length} / {s.max_players}</span>
          {:else if s.state === 'stopped'}
            <span class="stopped"><span class="dot"></span>Stopped</span>
          {:else}
            <span class="pill {s.state}" title={bootText(s) || undefined}>{stateText(s)}{s.exit_code != null ? ` (${s.exit_code})` : ''}</span>
          {/if}
        </div>
        <!-- One chip fits beside the footer's buttons; several get their own row. -->
        {#if nchips > 1}<div class="row chips">{@render chips(s, up)}</div>{/if}
        <!-- Players show once, in the header pill; the two meters get half the card each. -->
        <div class="meters">
          <div>
            <div class="t">CPU<b>{on ? `${Math.round(s.cpu)} %` : '—'}</b></div>
            <div class="bar"><i style:width="{on ? Math.min(100, s.cpu) : 0}%"></i></div>
          </div>
          <div title={on ? memTitle(mem, cap) : undefined}>
            <div class="t">RAM<b>{on ? bytes(mem) : '—'}{#if cap}<span class="u">{` / ${bytes(cap)}`}</span>{/if}</b></div>
            <div class="bar b mem">
              {#if span}<i style:width="{(Math.min(mem, cap || mem) / span) * 100}%"></i>{/if}
              {#if cap && mem > cap}<s style:left="{(cap / span) * 100}%"></s><u style:left="{(cap / span) * 100}%"></u>{/if}
            </div>
          </div>
        </div>
        <div class="row foot">
          <span class="left">
            {#if nchips === 1}{@render chips(s, up)}{/if}
            <span class="small subtle ell" title={s.players.join(', ')}>{s.players.join(', ') || (on ? 'Nobody online' : '')}</span>
          </span>
          {#if !on}
            <button class="btn sm go" onclick={() => power(s, 'start')}><Play size={13} />Start</button>
          {:else}
            <button class="btn sm icon ghost" title="Restart" onclick={() => power(s, 'restart')}><RotateCw size={14} /></button>
            <button class="btn sm icon ghost bad" title="Stop" onclick={() => power(s, 'stop')}><Square size={14} /></button>
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
  .meters { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
  .t { display: flex; align-items: baseline; gap: 8px; font-size: 11.5px; color: var(--muted); margin-bottom: 5px; white-space: nowrap; }
  .t b { margin-left: auto; color: var(--text); font-weight: 500; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .t .u { color: var(--muted); font-weight: 400; }
  /* Memory past the setting: a tick at the setting and a hatched tail. */
  .mem { position: relative; overflow: visible; }
  .mem s, .mem u { position: absolute; top: 0; bottom: 0; }
  .mem s { right: 0; border-radius: 0 9px 9px 0; background: repeating-linear-gradient(135deg, #38bdf8aa 0 3px, #38bdf833 3px 6px); }
  .mem u { top: -3px; bottom: -3px; width: 2px; margin-left: -1px; border-radius: 1px; background: var(--text); opacity: .7; }
  .foot { gap: 4px; }
  .foot .left { flex: 1; min-width: 0; display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
  /* Names take what the chips leave, down to a few letters, before wrapping. */
  .foot .left .ell { flex: 1 1 0; min-width: 40px; }
  .foot .btn { height: 26px; }
  .foot .btn.icon { width: 26px; justify-content: center; padding: 0; }
  .foot .btn + .btn { margin-left: 2px; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .empty { grid-column: 1 / -1; padding: 0; }
  @media (max-width: 760px) { .pad { padding: 16px; } .servers { grid-template-columns: 1fr; } }
</style>
