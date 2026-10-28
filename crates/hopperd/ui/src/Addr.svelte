<script>
  // A server's address as a chip with Copy: the header, Overview cards and
  // the Address card all show addresses with this.
  import { say } from './lib/toast.svelte.js';
  import { copyText } from './lib/browse.js';
  let { address, more = 0 } = $props();

  // copyText also works over plain http on the LAN (no clipboard API there).
  async function copy(e) {
    e.stopPropagation();
    e.preventDefault();
    say((await copyText(address)) ? `Copied ${address}` : `Couldn't copy: ${address}`, false);
  }
</script>

<span class="addr" title={address}><span class="mono a">{address}</span>{#if more}<span class="more">+{more}</span>{/if}<button type="button" class="cp" title="Copy the address" onclick={copy}>Copy</button></span>

<style>
  .addr { display: inline-flex; align-items: center; gap: 6px; border: 1px solid var(--line2); border-radius: 99px; padding: 0 0 0 9px; background: #ffffff06; max-width: 100%; vertical-align: middle; }
  .a { font-size: 11.5px; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .more { font-size: 11px; color: var(--muted); }
  .cp { font-size: 11px; color: var(--subtle); border-left: 1px solid var(--line2); padding: 1px 8px; border-radius: 0 99px 99px 0; }
  .cp:hover { background: var(--raise); color: var(--text); }
</style>
