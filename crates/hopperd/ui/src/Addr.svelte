<script>
  // A server's address as a chip with Copy: the header, Overview cards and
  // the Address card all show addresses with this.
  import { say } from './lib/toast.svelte.js';
  let { address, more = 0 } = $props();

  function copy(e) {
    e.stopPropagation();
    e.preventDefault();
    navigator.clipboard?.writeText(address).then(() => say(`Copied ${address}`), () => say(address));
  }
</script>

<span class="addr" title={address}><span class="mono a">{address}</span>{#if more}<span class="more">+{more}</span>{/if}<button type="button" class="cp" onclick={copy}>Copy</button></span>

<style>
  .addr { display: inline-flex; align-items: center; gap: 6px; border: 1px solid var(--line2); border-radius: 99px; padding: 0 0 0 9px; background: #ffffff06; max-width: 100%; vertical-align: middle; }
  .a { font-size: 11.5px; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .more { font-size: 11px; color: var(--muted); }
  .cp { font-size: 11px; color: var(--subtle); border-left: 1px solid var(--line2); padding: 1px 8px; border-radius: 0 99px 99px 0; }
  .cp:hover { background: var(--raise); color: var(--text); }
</style>
