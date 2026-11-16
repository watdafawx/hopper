<script>
  // Add a machine to this panel's list. A node comes first: it connects to
  // this panel by itself, so it needs no open port and no trusted
  // certificate. Signing in to a panel is the other way, for one the player
  // can't make a node, like a friend's (AddPanel.svelte). Only the real
  // owner adds nodes (a node runs code for this panel), so everyone else
  // goes straight to signing in.
  import { tick } from 'svelte';
  import { X, Copy, Plus, TriangleAlert, ChevronRight } from '@lucide/svelte';
  import { hello } from './lib/boot.svelte.js';
  import { session } from './lib/me.svelte.js';
  import { post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { copyText } from './lib/browse.js';
  import { CEILINGS, NODES_SHOWN, ceilingLabel, clock, defaultAddress } from './lib/nodes.js';
  import AddPanel from './AddPanel.svelte';

  let { onClose, onMade = () => {} } = $props();

  // Until hello says whether this panel can have nodes, guess from the role.
  const guess = session.trueOwner && NODES_SHOWN;
  let canNode = $state(guess);
  let mode = $state(guess ? 'node' : 'signin');
  let addr = $state('');
  let ceiling = $state('manage');
  let made = $state(null);
  // Most nodes are rented Linux boxes, whatever this browser runs on.
  let os = $state('linux');
  let busy = $state(false);
  let field = $state();

  hello().then(
    (h) => {
      canNode = session.trueOwner && NODES_SHOWN && !!h.capabilities?.includes('nodes');
      if (!canNode) mode = 'signin';
      if (!addr) addr = defaultAddress(location, h);
    },
    () => {},
  );

  $effect(() => {
    if (mode === 'node' && !made) tick().then(() => field?.focus());
  });

  async function make() {
    busy = true;
    try {
      made = await post('/nodes/keys', { url: addr.trim(), ceiling }, { home: true });
      onMade();
    } catch (e) {
      say(e.message, true);
    }
    busy = false;
  }

  async function copy() {
    if (await copyText(made[os])) say('Copied. Paste it on the other machine.');
    else say('Select the command and copy it.', true);
  }

  function keys(e) {
    if (mode === 'node' && e.key === 'Escape' && !busy && !e.defaultPrevented) {
      e.preventDefault();
      onClose();
    }
  }
</script>

<svelte:window onkeydown={keys} />

{#if mode === 'signin'}
  <AddPanel {onClose} onBack={canNode ? () => (mode = 'node') : null} />
{:else}
  <div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onClose()}>
    <div class="box popover" role="dialog" aria-modal="true" aria-labelledby="am-title">
      <h3 id="am-title">Add a machine<span class="sp"></span><button class="btn sm icon ghost" title="Close" aria-label="Close" onclick={onClose}><X size={14} /></button></h3>
      {#if !made}
        <p class="small subtle">Run Hopper Server on the other machine and make it a <b class="txt">node</b> of this panel. It connects here by itself: no open port, no certificate to set up. Its servers join this list.</p>
        <div class="step">
          <span class="n">1</span>
          <div class="col">
            <label class="small subtle" for="am-addr">How the other machine reaches this panel</label>
            <div class="input"><input id="am-addr" class="mono" bind:this={field} bind:value={addr} spellcheck="false" autocomplete="off" /></div>
          </div>
        </div>
        <div class="step">
          <span class="n">2</span>
          <div class="col">
            <span class="small subtle">What this panel may do there</span>
            <div class="seg" role="group" aria-label="What this panel may do there">
              {#each CEILINGS as c (c.id)}<button aria-pressed={ceiling === c.id} onclick={() => (ceiling = c.id)}>{c.label}</button>{/each}
            </div>
            <span class="tiny muted">{CEILINGS.find((c) => c.id === ceiling)?.blurb}</span>
            {#if ceiling === 'manage'}<span class="tiny warn"><TriangleAlert size={12} />Anyone who can sign in to this panel as an admin could run code on that machine.</span>{/if}
          </div>
        </div>
        <div class="step">
          <span class="n">3</span>
          <div class="col">
            <span class="small subtle">Make a command and paste it on the other machine (Linux or Windows). It installs Hopper Server and joins.</span>
            <button class="btn pri go-make" disabled={busy || !addr.trim()} onclick={make}><Plus size={14} />Make the join command</button>
            <span class="tiny muted">Or on that machine run <span class="mono sub">hopperd node join {addr.trim() || 'https://…'}</span> and allow its 6-digit code on Other machines.</span>
          </div>
        </div>
        <div class="hr"></div>
        <button class="alt row-hover" onclick={() => (mode = 'signin')}>
          <span class="grow"><b>Sign in to a panel instead</b><span class="tiny muted">For a panel you can't make a node, like a friend's. This browser signs in to it; this panel keeps only its address.</span></span>
          <ChevronRight size={15} />
        </button>
      {:else}
        <div class="utabs" role="tablist">
          <button role="tab" aria-selected={os === 'linux'} class:tab-sel={os === 'linux'} onclick={() => (os = 'linux')}>Linux</button>
          <button role="tab" aria-selected={os === 'windows'} class:tab-sel={os === 'windows'} onclick={() => (os = 'windows')}>Windows</button>
        </div>
        <p class="tiny muted">{os === 'linux' ? 'In a terminal on the other machine (it asks for your sudo password). It installs Hopper Server as a service and joins.' : 'In PowerShell, run as administrator, on the other machine. It installs Hopper Server as a service and joins.'}</p>
        <pre class="cmd mono">{made[os]}</pre>
        <p class="tiny muted">Works once, for 30 minutes (until {clock(made.expires)}). It is shown only now. It may: {ceilingLabel(ceiling)}. The machine shows up on Other machines within seconds of joining.</p>
        <div class="row end"><span class="sp"></span><button class="btn" onclick={onClose}>Done</button><button class="btn pri" onclick={copy}><Copy size={13} />Copy</button></div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .dialog-back { z-index: 80; }
  .box { width: min(540px, 100%); display: flex; flex-direction: column; gap: 14px; padding: 18px; }
  .box h3 { margin: 0; display: flex; align-items: center; gap: 8px; font-size: 15px; font-weight: 600; }
  .box p { margin: 0; }
  .txt { color: var(--text); }
  .step { display: grid; grid-template-columns: 22px minmax(0, 1fr); gap: 10px; align-items: start; }
  .n { width: 22px; height: 22px; border-radius: 50%; background: var(--raise); display: grid; place-items: center; font-size: 11.5px; font-weight: 600; color: var(--subtle); }
  .col { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
  .go-make { align-self: flex-start; }
  .sub { color: var(--subtle); overflow-wrap: anywhere; }
  /* A few values to pick from: the chosen one in the selected tint (desktop's widgets::Segmented). */
  .seg { display: inline-flex; gap: 2px; padding: 2px; border-radius: var(--radius); background: var(--hover); width: max-content; }
  .seg button { height: 26px; padding: 0 10px; border-radius: var(--radius-sm); color: var(--muted); font-size: 12.5px; }
  .seg button:hover { color: var(--text); }
  .seg button[aria-pressed='true'] { background: var(--sel); color: var(--text); font-weight: 600; }
  .seg button:focus-visible { outline: 2px solid var(--acc); outline-offset: -2px; }
  .warn { color: var(--subtle); display: flex; align-items: center; gap: 6px; }
  .warn :global(svg) { color: var(--gold); flex: none; }
  .hr { height: 1px; background: var(--line); }
  /* The other way in: a quiet row, not a second filled button. */
  .alt { display: flex; align-items: center; gap: 10px; width: 100%; text-align: left; padding: 10px 12px; border-radius: var(--radius-lg); border: 1px solid var(--line); color: var(--text); font: inherit; }
  .alt :global(svg) { color: var(--muted); flex: none; }
  .alt:focus-visible { outline: 2px solid var(--acc); outline-offset: 1px; }
  .grow { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
  .grow b { font-weight: 600; font-size: 13px; }
  .cmd { margin: 0; padding: 10px; border-radius: var(--radius); background: var(--term); border: 1px solid var(--line); font-size: 11.5px; white-space: pre-wrap; word-break: break-all; user-select: all; }
  .end { gap: 8px; }
</style>
