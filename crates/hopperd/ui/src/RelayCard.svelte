<script>
  // "Relay through a VPS" on a server's Overview (design/vps-relay.md, B2;
  // option A of design/mocks/vps-relay.html): the address players join
  // first, then whether the tunnel is up, the upload it saved against
  // vanilla, the upload now and the cache hits. Shown only for a server set
  // to use the relay. B4: the owner turns the relay's account checks on
  // for this server (Use the relay), off again, or plays at home while the
  // relay is down; each one restarts the server only after asking.
  import { Globe, Copy, Settings2 } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { copyText } from './lib/browse.js';
  import { say } from './lib/toast.svelte.js';
  import { session } from './lib/me.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';

  // `onSetup`: opens the setup dialog (repair, remove), for the owner.
  let { server, onSetup = null } = $props();
  let r = $state(null);
  let busy = $state(false);
  // The live feed replaces `server` every second; the id doesn't change.
  const sid = $derived(server.id);

  async function load() {
    try {
      r = await api(`/servers/${encodeURIComponent(sid)}/vps-relay`, { poll: true });
    } catch {
      // An older panel without the relay, or no access: no card.
      r = null;
    }
  }

  $effect(() => {
    sid;
    load();
    const t = setInterval(() => document.hidden || load(), 5000);
    return () => clearInterval(t);
  });

  const up = $derived(r?.state === 'up');
  // Upload: what home sent players through the relay against what vanilla
  // would have sent them.
  const total = $derived(r?.total ?? {});
  const saved = $derived(total.vanilla > 0 ? Math.max(0, Math.round((1 - total.sent / total.vanilla) * 100)) : null);
  const hitShare = $derived(total.raw > 0 ? Math.round((total.hit_bytes / total.raw) * 100) : null);

  function rate(bytesPerSec) {
    const bits = (bytesPerSec || 0) * 8;
    if (bits >= 1e6) return [(bits / 1e6).toFixed(1), 'Mbit/s'];
    if (bits >= 1e3) return [Math.round(bits / 1e3).toString(), 'kbit/s'];
    return [Math.round(bits).toString(), 'bit/s'];
  }
  const now = $derived(rate(r?.now?.sent));
  const nowVanilla = $derived(rate(r?.now?.vanilla).join(' '));
  const split = (s) => {
    const i = s.lastIndexOf(' ');
    return [s.slice(0, i), s.slice(i + 1)];
  };
  const hits = $derived(split(bytes(total.hit_bytes || 0)));

  function clock(unix) {
    if (!unix) return '';
    const d = new Date(unix * 1000);
    const today = new Date().toDateString() === d.toDateString();
    return today ? d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : d.toLocaleDateString([], { day: 'numeric', month: 'short' });
  }

  // The last hour, sent against vanilla, on one scale.
  const samples = $derived(r?.samples ?? []);
  const top = $derived(Math.max(1, ...samples.map((s) => s[2]), ...samples.map((s) => s[1])));
  const line = (k) => (samples.length < 2 ? '' : samples.map((s, i) => `${(i * 300) / (samples.length - 1)},${32 - (s[k] / top) * 30}`).join(' '));

  // The relay's words end a sentence before ours starts.
  const sentence = (t) => (/[.?!]$/.test(t) ? t : `${t}.`);

  async function copy() {
    say((await copyText(r.address)) ? 'Copied the address' : "Couldn't copy the address", false);
  }
  // Where the server stands with the relay's account checks (B4).
  const fw = $derived(r?.forward ?? null);
  const mode = $derived(fw?.mode ?? 'direct');
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  let doing = $state('');

  // What each change does, in the player's words; the restart is said
  // plainly and only happens when the dialog is answered yes.
  const ASK = {
    use: {
      title: 'Use the relay for this server?',
      body: (m) => `Players join through ${r.address} and the relay checks their accounts. Hopper adds ${m}${m === 'FabricProxy-Lite' ? ' (with Fabric API, which it needs, when the server has none)' : ''}, writes its settings and sets four server settings the relay needs (online mode, secure chat profiles, compression, the server's address). Every change is in History.`,
      ok: 'Use the relay',
    },
    stop: {
      title: 'Stop using the relay for this server?',
      body: (m) => `Hopper removes the mods it added for the relay and puts the four server settings back as they were. Players join with this server's own address again.`,
      ok: 'Stop using the relay',
    },
    'play-at-home': {
      title: 'Play at home until the relay is back?',
      body: (m) => `Players join with this server's own address, and the server checks their accounts again. Hopper turns ${m} off and puts the four server settings back. When the relay is back, this card offers to switch back.`,
      ok: 'Play at home',
    },
    'back-to-relay': {
      title: 'Go back to the relay?',
      body: (m) => `The relay is connected again. Players join through ${r.address} and the relay checks their accounts. Hopper turns ${m} on and sets the four server settings again.`,
      ok: 'Use the relay',
    },
  };

  async function change(action) {
    const a = ASK[action];
    const restart = alive;
    const body = `${a.body(fw.mod || 'the relay mod')} ${restart ? 'The server restarts for this: Hopper stops it, makes the change and starts it again. Players online now are disconnected.' : 'It takes effect when the server starts.'}`;
    if (!(await confirmIt(a.title, { body, ok: restart ? `Restart and ${a.ok.toLowerCase()}` : a.ok }))) return;
    doing = 'Starting';
    try {
      const { job } = await post(`/servers/${encodeURIComponent(sid)}/vps-relay/forward`, { action, restart });
      await followJob(job, (st) => (doing = st.detail || 'Working'));
      say(action === 'stop' ? 'Stopped using the relay' : action === 'play-at-home' ? 'Playing at home' : 'Using the relay', false);
    } catch (e) {
      say(e.message, true);
    } finally {
      doing = '';
      load();
    }
  }

  async function retry() {
    busy = true;
    try {
      await post('/vps-relay/retry', {});
      setTimeout(load, 1500);
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }
</script>

{#if r?.relayed}
  <div class="card relay">
    <h3>
      <Globe size={14} /><span class="t">Relay through a VPS</span>
      {#if up}<span class="pill running"><span class="glow"></span>Connected</span>
      {:else if r.state === 'connecting'}<span class="pill starting"><span class="glow"></span>Connecting</span>
      {:else if r.state === 'off'}<span class="pill">Off</span>
      {:else}<span class="pill crashed"><span class="glow"></span>Not connected</span>{/if}
      <span class="sp"></span>
      {#if up}{@const when = `since ${clock(r.since)}${r.rtt_ms != null ? ` · ${r.rtt_ms < 1 ? 'under 1' : r.rtt_ms} ms to the relay` : ''}`}<span class="tiny muted when" title={when}>{when}</span>
      {:else if r.state === 'down' && r.retry_in != null}<span class="tiny muted when">{r.retry_in > 0 ? `trying again in ${r.retry_in} s` : 'trying again now'}</span>{/if}
      {#if onSetup}<button class="btn ghost sm icon" title="Relay setup: check, repair or remove it" aria-label="Relay setup" onclick={onSetup}><Settings2 size={14} /></button>{/if}
    </h3>
    <!-- At home the relay's address doesn't reach the server: no account checks there. -->
    {#if r.address && mode !== 'home'}
      <div class="lbl">Players join</div>
      <div class="addr"><span class="mono">{r.address}</span><button class="btn sm" onclick={copy}><Copy size={13} />Copy</button></div>
    {/if}
    {#if fw && mode === 'home'}
      <!-- Playing at home: the way back is offered once the relay is up again. -->
      <div class="note" class:back={up}>{up ? 'The relay is back.' : 'Playing at home while the relay is down.'} Players join with this server's own address and the server checks their accounts.{#if up}{' '}Go back to the relay so players join through it again.{/if}</div>
      {#if session.owner}
        <div class="row acts">
          {#if up}<button class="btn pri" disabled={!!doing} onclick={() => change('back-to-relay')}>Go back to the relay</button>{/if}
          <button class="btn" disabled={!!doing} onclick={() => change('stop')}>Stop using the relay</button>
          {#if doing}<span class="tiny muted">{doing}…</span>{/if}
        </div>
      {/if}
    {:else if r.state === 'down'}
      <div class="note">{sentence(r.error || "Can't reach the relay.")} The server keeps running here; players on the relay see "Home server is offline".</div>
      <div class="row acts">
        {#if session.owner}<button class="btn" disabled={busy} onclick={retry}>Try now</button>{/if}
        {#if session.owner && mode === 'relay'}<button class="btn" disabled={!!doing} onclick={() => change('play-at-home')}>Play at home until it's back</button>{/if}
        {#if doing}<span class="tiny muted">{doing}…</span>{/if}
      </div>
    {:else if r.state === 'off'}
      <div class="note">The relay is turned off in this panel's settings. Players can't join through it.</div>
    {:else}
      <div class="stats">
        <div class="stat"><div class="lbl">Upload saved</div><div class="v">{saved ?? '—'}{#if saved != null} <span class="u">%</span>{/if}</div><div class="s">{bytes(total.sent || 0)} sent · {bytes(total.vanilla || 0)} vanilla</div></div>
        <div class="stat"><div class="lbl">Upload now</div><div class="v">{now[0]} <span class="u">{now[1]}</span></div><div class="s">vanilla {nowVanilla}</div></div>
        <div class="stat"><div class="lbl">Cache hits</div><div class="v">{hits[0]} <span class="u">{hits[1]}</span></div><div class="s">{hitShare != null ? `${hitShare}% of what was sent` : 'nothing sent yet'}</div></div>
      </div>
      {#if samples.length >= 2}
        <svg class="spark" viewBox="0 0 300 34" preserveAspectRatio="none" aria-hidden="true">
          <polyline fill="none" stroke="var(--muted)" stroke-opacity=".5" stroke-width="1.5" vector-effect="non-scaling-stroke" points={line(2)} />
          <polyline fill="none" stroke="var(--acc)" stroke-width="1.5" vector-effect="non-scaling-stroke" points={line(1)} />
        </svg>
        <div class="row tiny muted legend"><span><i class="sw sent"></i>sent</span><span><i class="sw"></i>vanilla would send</span></div>
      {/if}
      {#if fw && session.owner}
        <!-- The relay's account checks for this server (Velocity on the VPS). -->
        <div class="fwd">
          {#if mode === 'relay'}
            <span class="pill running">On</span><span class="small">Accounts are checked by the relay{fw.mod ? ` · ${fw.mod}` : ''}</span>
            <span class="sp"></span>
            <button class="btn sm" disabled={!!doing} onclick={() => change('stop')}>Stop using the relay</button>
          {:else if fw.why_not}
            <span class="small subtle">{sentence(fw.why_not)}</span>
          {:else if !fw.velocity}
            <span class="small subtle">Repair the relay setup so the relay can check accounts for this server.</span>
            <span class="sp"></span>
            {#if onSetup}<button class="btn sm" onclick={onSetup}>Repair</button>{/if}
          {:else}
            <span class="small subtle">Let the relay check accounts, so players join through it{fw.mod ? `. Adds ${fw.mod}${fw.mod === 'FabricProxy-Lite' ? ' (and Fabric API if needed)' : ''}` : ''}.</span>
            <span class="sp"></span>
            <button class="btn sm" disabled={!!doing} onclick={() => change('use')}>Use the relay</button>
          {/if}
          {#if doing}<span class="tiny muted">{doing}…</span>{/if}
        </div>
      {/if}
    {/if}
  </div>
{/if}

<style>
  .relay { margin-bottom: 12px; flex: none; }
  /* One line at any width: the status text gives way (ellipsis) before the
     title, the pill or the settings button move. */
  h3 { flex-wrap: nowrap; min-width: 0; }
  h3 > :global(*) { flex: none; }
  h3 .t { white-space: nowrap; }
  h3 .sp { flex: 1 1 0; min-width: 0; }
  h3 .when { flex: 0 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sp { flex: 1; }
  .lbl { margin-bottom: 4px; }
  .addr { display: flex; align-items: center; gap: 8px; background: var(--base); border: 1px solid var(--line2); border-radius: var(--radius); padding: 6px 6px 6px 10px; }
  .addr .mono { font-size: 13px; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .stats { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px; margin-top: 12px; }
  .stat .v { font-size: 18px; font-weight: 650; font-variant-numeric: tabular-nums; }
  .stat .v .u { font-size: 12px; color: var(--muted); font-weight: 500; }
  .stat .s { font-size: 11.5px; color: var(--muted); }
  .spark { width: 100%; height: 34px; display: block; margin-top: 10px; }
  .row { display: flex; align-items: center; gap: 8px; }
  .legend { gap: 14px; margin-top: 4px; }
  /* A coloured line for each series; the words stay in the readable muted grey. */
  .sw { display: inline-block; width: 12px; height: 2px; border-radius: 1px; background: var(--muted); opacity: .6; vertical-align: middle; margin-right: 5px; }
  .sw.sent { background: var(--acc); opacity: 1; }
  .note { font-size: 12px; color: var(--subtle); background: var(--hover); border-radius: var(--radius); padding: 8px 10px; margin-top: 10px; }
  .acts { margin-top: 10px; flex-wrap: wrap; }
  .note.back { color: var(--text); background: var(--green-dim); }
  .fwd { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-top: 12px; padding-top: 10px; border-top: 1px solid var(--line); }
  @media (max-width: 520px) { .stats { grid-template-columns: 1fr 1fr; } }
</style>
