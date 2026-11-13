<script>
  // Sign in to another panel, inside its group in the server list. Its
  // password goes straight to that panel; this page keeps only the browser
  // session it hands back (in this browser, never on this panel).
  import { tick } from 'svelte';
  import { signIn } from './lib/panels.svelte.js';

  let { panel } = $props();

  let name = $state('');
  let password = $state('');
  let code = $state('');
  let needCode = $state(false);
  let keep = $state(true);
  let busy = $state(false);
  let error = $state('');
  let codeEl = $state();

  async function submit(e) {
    e.preventDefault();
    if (busy || !password) return;
    busy = true;
    error = '';
    try {
      await signIn(panel, { name: name.trim(), password, code: needCode ? code.trim() : null, keep });
      password = '';
      code = '';
    } catch (err) {
      if (err.data?.need_code) {
        needCode = true;
        await tick();
        codeEl?.focus();
      } else error = err.message;
    } finally {
      busy = false;
    }
  }
</script>

<form class="si" onsubmit={submit}>
  <div class="small">{panel.reason === 'expired' ? `Your sign-in to ${panel.name} ended. Sign in again.` : panel.reason === 'out' ? `Signed out of ${panel.name}.` : `Sign in to ${panel.name}.`}</div>
  <div class="input sm"><input bind:value={name} autocomplete="username" spellcheck="false" placeholder="Name (empty: owner)" aria-label="Name on {panel.name}" /></div>
  <div class="input sm"><input type="password" bind:value={password} autocomplete="current-password" placeholder="Password" aria-label="Password on {panel.name}" /></div>
  {#if needCode}
    <div class="input sm"><input bind:this={codeEl} bind:value={code} inputmode="numeric" autocomplete="one-time-code" maxlength="6" placeholder="Code from your app" aria-label="Two-factor code" /></div>
  {/if}
  <label class="row tiny subtle"><input type="checkbox" checked={!keep} onchange={(e) => (keep = !e.target.checked)} />Don't keep me signed in on this browser</label>
  {#if error}<div class="tiny err" role="alert">{error}</div>{/if}
  <button class="btn sm pri" disabled={busy || !password}>{busy ? 'Signing in…' : 'Sign in'}</button>
</form>

<style>
  .si { display: flex; flex-direction: column; gap: 6px; padding: 6px 10px 10px 30px; }
  .si .btn { align-self: flex-start; }
  .row { gap: 6px; }
  .err { color: var(--red); }
</style>
