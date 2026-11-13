<script>
  // Add another Hopper Server to this page's server list. The address is
  // checked first, step by step (it answers, its certificate is trusted, it
  // lists this page), and whatever fails says how to fix it, in order. It
  // is then signed in to from its group in the list.
  import { tick } from 'svelte';
  import { Check, X, Copy, ExternalLink, LoaderCircle, Download } from '@lucide/svelte';
  import { hello } from './lib/boot.svelte.js';
  import { check, addPanel } from './lib/panels.svelte.js';
  import { say } from './lib/toast.svelte.js';

  let { onClose } = $props();

  let typed = $state('');
  let busy = $state(false);
  let res = $state(null);
  let field = $state();
  const origin = location.origin;

  // The panel's own certificate authority (`/api/v1/ca.crt`, open). Asked
  // for over plain http on the same port: the panel answers that on your
  // own network and Tailscale, where its https would show the very warning
  // this is meant to fix. Elsewhere it asks for https, so the download then
  // needs its page opened once.
  const caUrl = (url) => {
    try {
      const u = new URL(url);
      return `http://${u.host}/api/v1/ca.crt`;
    } catch {
      return `${url}/api/v1/ca.crt`;
    }
  };

  // "Add it as a node instead" (design/multi-panel.md, A) only shows once
  // this panel can have nodes; until then nothing points at it.
  let nodes = $state(false);
  hello().then((h) => (nodes = !!h.capabilities?.includes('nodes')), () => {});

  $effect(() => {
    tick().then(() => field?.focus());
  });

  async function run(e) {
    e?.preventDefault();
    if (!typed.trim() || busy) return;
    busy = true;
    res = null;
    try {
      const r = await check(typed);
      if (r.ok) {
        await addPanel(r.url, r.name);
        say(`Added ${r.name}. Sign in to it in the server list.`);
        onClose();
        return;
      }
      res = r;
    } finally {
      busy = false;
    }
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(origin);
      say(`Copied ${origin}`);
    } catch {
      say("Can't copy here.", true);
    }
  }

  const who = $derived(res?.name || 'that panel');

  function keys(e) {
    if (e.key === 'Escape' && !busy) {
      e.preventDefault();
      onClose();
    }
  }
</script>

<svelte:window onkeydown={keys} />

{#snippet caHelp(url)}
  <a href={caUrl(url)} download>download its certificate <Download size={11} /></a> and install it as a trusted root certificate, then restart the browser and press Check again. Hopper Server keeps it across restarts, so once is enough.
  <details class="how">
    <summary>How to install it</summary>
    <ul>
      <li><b>Windows</b> (Chrome, Edge): open the file › Install Certificate › Current User › Place all certificates in the following store › Trusted Root Certification Authorities.</li>
      <li><b>Mac</b> (Safari, Chrome): open the file, then in Keychain Access double-click it › Trust › Always Trust.</li>
      <li><b>Firefox</b>: Settings › Privacy & Security › View Certificates › Authorities › Import, and tick "Trust this CA to identify websites".</li>
    </ul>
    <p class="tiny muted">Only install it for a panel you run: this PC then trusts every certificate that panel makes. If the download doesn't start, open its own page first.</p>
  </details>
{/snippet}

{#snippet asNode()}
  {#if nodes}
    <p class="small subtle">Or add it as a node of this panel instead: it connects to this panel itself, so its certificate and address don't matter. Settings › Nodes.</p>
  {/if}
{/snippet}

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onClose()}>
  <div class="box popover" role="dialog" aria-modal="true" aria-labelledby="ap-title">
    <form class="f" onsubmit={run}>
      <h3 id="ap-title">Add a Hopper Server</h3>
      <p class="small subtle">Its servers join this list, next to this panel's. You sign in to it once on this browser; this panel only keeps its address.</p>
      <div class="frow">
        <label class="small" for="ap-addr">Address</label>
        <div class="input"><input id="ap-addr" bind:this={field} bind:value={typed} placeholder="rig:8790" spellcheck="false" autocomplete="off" oninput={() => (res = null)} /></div>
      </div>

      {#if res?.steps?.length}
        <ul class="steps">
          {#each res.steps as s, i (i)}
            <li class:bad={s.ok === false}>
              <span class="ck">{#if s.ok}<Check size={13} />{:else}<X size={13} />{/if}</span>{s.text}
            </li>
          {/each}
        </ul>
      {/if}

      {#if res}
        <div class="fix" role="alert">
          {#if res.problem === 'bad'}
            <p>That isn't an address. Type it like <span class="mono">rig:8790</span> or <span class="mono">https://panel.example.com</span>, with nothing after the port.</p>
          {:else if res.problem === 'self'}
            <p>That's this panel. Its servers are already in the list.</p>
          {:else if res.problem === 'dup'}
            <p>{res.name} is already in the list.</p>
          {:else if res.problem === 'notlisted'}
            <p><b>It answers, but doesn't let this page in yet.</b> To fix it:</p>
            <ol>
              <li><a href={res.url} target="_blank" rel="noopener">Open its own page <ExternalLink size={11} /></a> and sign in as its owner.</li>
              <li>In Settings › Access & devices › Other web pages, add this page:
                <span class="orig"><span class="mono">{origin}</span><button type="button" class="btn sm ghost icon" title="Copy" aria-label="Copy this page's address" onclick={copy}><Copy size={12} /></button></span>
              </li>
              <li>Come back here and press Check again.</li>
            </ol>
          {:else if res.problem === 'cert'}
            <p><b>Your browser doesn't trust its certificate yet.</b> Either:</p>
            <ol>
              <li><b>This browser, for now:</b> <a href={res.url} target="_blank" rel="noopener">open its own page <ExternalLink size={11} /></a>, accept the certificate warning, and press Check again.</li>
              <li><b>This PC, for good:</b> {@render caHelp(res.url)}</li>
            </ol>
            {@render asNode()}
          {:else if res.problem === 'unreachable'}
            <p><b>Can't reach it.</b> Check the address and port, and that the panel is on and this PC can reach it (same network or Tailscale).</p>
            {#if res.maybeCert}
              <p class="small subtle">If it uses https with its own certificate, <a href={res.maybeCert} target="_blank" rel="noopener">open it once <ExternalLink size={11} /></a> and accept the warning, then check again. Or trust it on this PC for good: {@render caHelp(res.maybeCert)}</p>
            {/if}
            {@render asNode()}
          {:else if res.problem === 'mixed'}
            <p><b>This page is secure (https) and that panel isn't</b>, so the browser blocks it. Open this page over http, or turn on https on that panel (Settings › Access & devices › Certificate).</p>
            {@render asNode()}
          {:else if res.problem === 'old'}
            <p><b>{who} runs Hopper Server {res.version || ''}</b>, which can't be used from another page. Update it on <a href={res.url} target="_blank" rel="noopener">its own page <ExternalLink size={11} /></a>, then check again.</p>
          {:else if res.problem === 'setup'}
            <p><b>{who} isn't set up yet.</b> <a href={res.url} target="_blank" rel="noopener">Open its own page <ExternalLink size={11} /></a> to make its owner account, then check again.</p>
          {/if}
        </div>
      {/if}

      <div class="row end">
        <button type="button" class="btn" disabled={busy} onclick={onClose}>Cancel</button>
        <button class="btn pri" disabled={busy || !typed.trim()}>{#if busy}<LoaderCircle size={13} class="spin" />Checking…{:else}{res ? 'Check again' : 'Check and add'}{/if}</button>
      </div>
    </form>
  </div>
</div>

<style>
  .dialog-back { z-index: 80; }
  .box { width: min(480px, 100%); padding: 18px; }
  .f { display: flex; flex-direction: column; gap: 12px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .f p { margin: 0; }
  .frow { display: grid; grid-template-columns: 72px minmax(0, 1fr); align-items: center; gap: 10px; }
  .steps { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 5px; font-size: 12.5px; }
  .steps li { display: flex; align-items: center; gap: 7px; color: var(--subtle); }
  .ck { display: grid; place-items: center; width: 18px; height: 18px; border-radius: 50%; background: var(--green-dim); color: var(--green); flex: none; }
  .steps li.bad { color: var(--text); }
  .steps li.bad .ck { background: var(--red-dim); color: var(--red); }
  .fix { display: flex; flex-direction: column; gap: 8px; font-size: 13px; border-radius: var(--radius-lg); background: var(--raise); padding: 10px 12px; }
  .fix ol { margin: 0; padding-left: 20px; display: flex; flex-direction: column; gap: 6px; }
  /* Inline and unbroken, not inline-flex: on a phone the comma after a link
     stays with it, and its icon never sits on a line of its own. */
  .fix a { color: color-mix(in srgb, var(--acc) 60%, var(--text)); white-space: nowrap; }
  .fix a :global(svg) { vertical-align: -1px; margin-left: 2px; }
  .orig { display: flex; align-items: center; gap: 4px; margin-top: 4px; padding: 2px 4px 2px 8px; border-radius: var(--radius); background: var(--base); border: 1px solid var(--line); width: fit-content; max-width: 100%; }
  .orig .mono { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12px; }
  .how { margin-top: 6px; }
  .how summary { cursor: pointer; color: var(--subtle); font-size: 12.5px; width: fit-content; }
  .how ul { margin: 6px 0 0; padding-left: 18px; display: flex; flex-direction: column; gap: 4px; font-size: 12.5px; }
  .how p { margin-top: 6px; }
  .end { justify-content: flex-end; }
  :global(.spin) { animation: spin .8s linear infinite; }
  @media (max-width: 480px) { .frow { grid-template-columns: 1fr; gap: 4px; } }
</style>
