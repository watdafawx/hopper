<script>
  // The strip under the panel: how this browser reaches it (LAN, Tailscale,
  // internet), whether it's encrypted, and the round trip. The route comes
  // from the panel (`via` in /hello), which sees the real address.
  import { Wifi, WifiOff, Waypoints, Globe, Lock } from '@lucide/svelte';
  import { live } from './lib/live.svelte.js';
  import { routeKind } from './lib/format.js';

  const kind = $derived(routeKind(live.via));
  const Icon = $derived({ LAN: Wifi, Local: Wifi, Tailscale: Waypoints, Internet: Globe }[kind]);
  const tls = location.protocol === 'https:';
  // Matches the ping bars' yellow (hopper::server::signal_bars: under 300 ms is 4+ bars).
  const slow = $derived(live.ping != null && live.ping >= 300);
  const tint = $derived(!live.connected || slow ? 'warn' : { LAN: 'lan', Local: 'lan', Tailscale: 'ts', Internet: 'acc' }[kind]);
  // Nothing heard yet: the page is still opening, nothing was lost.
  const first = $derived(live.via == null);
  const where = $derived({ Local: 'On this PC', LAN: 'On your network', Tailscale: 'Over Tailscale', Internet: 'Over the internet' }[kind]);
</script>

<footer class="sbar">
  <a class="chip {tint}" href="#/settings/access" title="How this browser reaches the panel">
    {#if live.connected}
      {#if tls}<Lock size={12} />{/if}<Icon size={12} />{kind}{#if live.ping != null}<span class="ms">· {live.ping} ms</span>{/if}
    {:else}
      <WifiOff size={12} />{first ? 'Connecting…' : 'Reconnecting…'}
    {/if}
  </a>
  <span class="txt">{live.connected ? `${where} · ${tls ? 'encrypted' : 'not encrypted (http)'}` : first ? '' : 'Lost the connection. Trying again…'}</span>
</footer>

<style>
  .sbar { flex: none; height: 28px; display: flex; align-items: center; gap: 10px; padding: 0 10px; background: var(--side); border-top: 1px solid var(--side-line); font-size: 11.5px; }
  .chip { text-decoration: none; border-radius: 999px; }
  .ms { opacity: 0.75; }
  .txt { color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
</style>
