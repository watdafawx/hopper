<script>
  // "Relay through a VPS" on a server's Overview (design/vps-relay.md, B2;
  // option A of design/mocks/vps-relay.html): the address players join
  // first, then whether the tunnel is up, the upload it saved against
  // vanilla, the upload now and the cache hits. Shown only for a server set
  // to use the relay.
  import { Globe, Copy } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { copyText } from './lib/browse.js';
  import { say } from './lib/toast.svelte.js';
  import { session } from './lib/me.svelte.js';

  let { server } = $props();
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
      <Globe size={14} />Relay through a VPS
      {#if up}<span class="pill running"><span class="glow"></span>Connected</span>
      {:else if r.state === 'connecting'}<span class="pill starting"><span class="glow"></span>Connecting</span>
      {:else if r.state === 'off'}<span class="pill">Off</span>
      {:else}<span class="pill crashed"><span class="glow"></span>Not connected</span>{/if}
      <span class="sp"></span>
      {#if up}<span class="tiny muted">since {clock(r.since)}{#if r.rtt_ms != null} · {r.rtt_ms < 1 ? 'under 1' : r.rtt_ms} ms to the relay{/if}</span>
      {:else if r.state === 'down' && r.retry_in != null}<span class="tiny muted">{r.retry_in > 0 ? `trying again in ${r.retry_in} s` : 'trying again now'}</span>{/if}
    </h3>
    {#if r.address}
      <div class="lbl">Players join</div>
      <div class="addr"><span class="mono">{r.address}</span><button class="btn sm" onclick={copy}><Copy size={13} />Copy</button></div>
    {/if}
    {#if r.state === 'down'}
      <div class="note">{sentence(r.error || "Can't reach the relay.")} The server keeps running here; players on the relay see "Home server is offline".</div>
      <div class="row acts">
        {#if session.owner}<button class="btn" disabled={busy} onclick={retry}>Try now</button>{/if}
        <!-- Switching players back to home's own address is B4. -->
        <button class="btn" disabled title="Coming in a later version">Play at home until it's back</button>
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
    {/if}
  </div>
{/if}

<style>
  .relay { margin-bottom: 12px; flex: none; }
  h3 { flex-wrap: wrap; }
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
  @media (max-width: 520px) { .stats { grid-template-columns: 1fr 1fr; } }
</style>
