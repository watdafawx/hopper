<script>
  // Settings › Relays (design/vps-relay.md, B7; option A of the B7 mock):
  // each relay this panel uses, and under it the servers that go through
  // it, in the row pattern of Settings › Other machines. One relay per
  // panel for now. A server is set up for the relay from its own page
  // (More › Relay through a VPS); its Overview card has the details.
  import { onMount } from 'svelte';
  import { Globe } from '@lucide/svelte';
  import { api, put } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';

  let data = $state(null);
  let now = $state(Date.now() / 1000);

  async function load() {
    try {
      data = await api('/vps-relays', { poll: true });
      now = Date.now() / 1000;
    } catch (e) {
      if (!data) say(e.message, true);
    }
  }

  onMount(() => {
    load();
    const t = setInterval(() => document.hidden || load(), 5000);
    return () => clearInterval(t);
  });

  const STATE = {
    up: { light: 'running', chip: 'Connected', cls: 'running' },
    connecting: { light: 'starting', chip: 'Connecting', cls: 'starting' },
    off: { light: '', chip: 'Off', cls: '' },
    down: { light: 'crashed', chip: 'Not connected', cls: 'crashed' },
  };

  function since(t) {
    const s = Math.max(0, now - t);
    if (s < 90) return 'a minute';
    if (s < 3600) return `${Math.round(s / 60)} min`;
    if (s < 86400 * 2) return `${Math.round(s / 3600)} h`;
    return `${Math.round(s / 86400)} days`;
  }

  function relayLine(r) {
    if (r.state === 'up') {
      const parts = [`Connected for ${since(r.since)}`];
      if (r.rtt_ms != null) parts.push(`ping ${r.rtt_ms < 1 ? 'under 1' : r.rtt_ms} ms`);
      if (r.relay_version) parts.push(`Hopper Server ${r.relay_version}`);
      return { text: parts.join(' · '), bad: false };
    }
    if (r.state === 'off') return { text: "Turned off in this panel's settings. Players can't join through it.", bad: false };
    if (r.state === 'connecting') return { text: 'Connecting…', bad: false };
    return { text: r.error || "Can't reach the relay.", bad: true };
  }

  const CHIP = { relay: 'Accounts checked by the relay', home: 'Playing at home', direct: 'Server checks accounts' };

  function serverLine(s, r) {
    const a = s.addresses ?? {};
    if (a.state === 'none') return { text: "It has no address of its own, so players can't pick it on this relay. Give it one in its Settings › Network & address.", bad: true };
    if (a.state === 'stale') return { text: `Its address ${a.hosts?.[0] ?? ''} isn't on the relay yet. Run the relay setup again (Repair on its Overview).`, bad: true };
    const parts = [s.mode === 'home' ? 'Players join with its own address for now' : `Players join at ${s.address}`];
    if (s.voice) parts.push('voice chat through the relay');
    else if (r.voice_owner) parts.push(`voice chat goes through the relay for ${r.voice_owner} only`);
    return { text: parts.join(' · '), bad: false };
  }

  async function takeOff(r, s) {
    const ok = await confirmIt(`Take ${s.name} off the relay?`, {
      body: `Players can't join ${s.name} through ${r.address} any more; they join with its own address. The other servers on the relay keep going as they are. You can set it up again from its page (More › Relay through a VPS).`,
      ok: 'Take it off',
    });
    if (!ok) return;
    try {
      await put('/vps-relay', { servers: r.servers.filter((x) => x.id !== s.id).map((x) => x.id) });
      say(`Took ${s.name} off the relay`, false);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }
</script>

<div class="relays">
  {#each data?.relays ?? [] as r (r.address)}
    {@const st = STATE[r.state] ?? STATE.down}
    {@const line = relayLine(r)}
    <div class="card list">
      <div class="rhead">
        <span class="dot {st.light}"></span>
        <div class="main">
          <div class="nm" title={r.address}>{r.address}</div>
          <div class="ln tiny" class:bad={line.bad} class:muted={!line.bad}>{line.text}</div>
        </div>
        <span class="chip {st.cls}">{st.chip}</span>
      </div>
      <div class="lbl count">{r.servers.length === 1 ? '1 server' : `${r.servers.length} servers`}</div>
      {#each r.servers as s (s.id)}
        {@const l = serverLine(s, r)}
        <div class="mrow row-hover">
          <div class="main">
            <a class="nm" href="#/server/{encodeURIComponent(s.id)}" title={s.name}>{s.name}</a>
            <div class="ln tiny" class:bad={l.bad} class:muted={!l.bad}>{l.text}</div>
          </div>
          <div class="meta">
            <span class="chip">{CHIP[s.mode] ?? CHIP.direct}</span>
            <span class="cnt tiny muted">{s.players === 1 ? '1 player' : `${s.players} players`}</span>
            <span class="act">
              <!-- One that checks accounts through the relay stops using it first, on its card. -->
              {#if s.mode === 'direct'}<button class="btn sm" onclick={() => takeOff(r, s)}>Take off</button>{/if}
            </span>
          </div>
        </div>
      {/each}
    </div>
  {:else}
    {#if data}
      <div class="card"><div class="empty-state"><div class="es-ic"><Globe size={18} /></div><b>No relay yet</b><p>A relay is a VPS your players join through. Set one up from a server's page: More › Relay through a VPS.</p></div></div>
    {:else}
      <div class="card"><div class="skel"><i></i></div><div class="skel"><i></i></div></div>
    {/if}
  {/each}
</div>

<style>
  .relays { display: flex; flex-direction: column; gap: 14px; }
  .list { padding: 10px 6px 4px; }
  .rhead { display: flex; align-items: center; gap: 12px; padding: 4px 8px 10px; }
  .count { padding: 4px 8px 6px; margin: 0; }
  /* One server: its name over how players reach it, then who checks
     accounts, its players and what to do. Same columns as Other machines. */
  .mrow { display: grid; grid-template-columns: minmax(0, 1fr) auto; grid-template-areas: 'main meta'; column-gap: 12px; align-items: center; padding: 10px 8px; border-top: 1px solid var(--line); }
  .main { grid-area: main; min-width: 0; display: flex; flex-direction: column; gap: 2px; flex: 1; }
  .nm { font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  a.nm { color: var(--text); }
  a.nm:hover { color: var(--acc); }
  .ln { overflow-wrap: anywhere; }
  .bad { color: var(--red-text); }
  .meta { grid-area: meta; display: flex; align-items: center; gap: 10px; }
  .cnt { min-width: 64px; text-align: right; white-space: nowrap; }
  .act { min-width: 74px; display: flex; justify-content: flex-end; }
  .list:not(:has(.act .btn)) .act { display: none; }
  .empty-state :global(svg) { display: block; }
  @media (max-width: 600px) {
    .mrow { grid-template-columns: minmax(0, 1fr); grid-template-areas: 'main' 'meta'; row-gap: 6px; }
    .cnt, .act { min-width: 0; text-align: left; }
    .meta { flex-wrap: wrap; }
  }
</style>
