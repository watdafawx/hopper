<script>
  // Add another Hopper Server to this page's server list. The address is
  // checked first, step by step (it answers, its certificate is trusted, it
  // lists this page), and whatever fails says how to fix it, in order. It
  // is then signed in to from its group in the list.
  import { tick } from 'svelte';
  import { Check, X, Copy, ExternalLink, LoaderCircle } from '@lucide/svelte';
  import { check, addPanel } from './lib/panels.svelte.js';
  import { say } from './lib/toast.svelte.js';

  let { onClose } = $props();

  let typed = $state('');
  let busy = $state(false);
  let res = $state(null);
  let field = $state();
  const origin = location.origin;

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
            <p><b>Your browser doesn't trust its certificate yet.</b> To fix it:</p>
            <ol>
              <li><a href={res.url} target="_blank" rel="noopener">Open its own page <ExternalLink size={11} /></a> and accept the certificate warning.</li>
              <li>Come back here and press Check again.</li>
            </ol>
          {:else if res.problem === 'unreachable'}
            <p><b>Can't reach it.</b> Check the address and port, and that the panel is on and this PC can reach it (same network or Tailscale).</p>
            {#if res.maybeCert}
              <p class="small subtle">If it uses https with its own certificate, <a href={res.maybeCert} target="_blank" rel="noopener">open it once <ExternalLink size={11} /></a> and accept the warning, then check again.</p>
            {/if}
          {:else if res.problem === 'mixed'}
            <p><b>This page is secure (https) and that panel isn't</b>, so the browser blocks it. Open this page over http, or turn on https on that panel (Settings › Access & devices › Certificate).</p>
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
  .fix a { color: var(--acc); display: inline-flex; align-items: center; gap: 3px; }
  .orig { display: flex; align-items: center; gap: 4px; margin-top: 4px; padding: 2px 4px 2px 8px; border-radius: var(--radius); background: var(--base); border: 1px solid var(--line); width: fit-content; max-width: 100%; }
  .orig .mono { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12px; }
  .end { justify-content: flex-end; }
  :global(.spin) { animation: spin .8s linear infinite; }
  @media (max-width: 480px) { .frow { grid-template-columns: 1fr; gap: 4px; } }
</style>
