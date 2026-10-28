<script>
  import { KeyRound, Lock, User, CircleCheck } from '@lucide/svelte';
  import { post } from './lib/api.js';

  let { hello, onDone } = $props();
  let code = $state('');
  let password = $state('');
  let confirm = $state('');
  let busy = $state(false);
  let error = $state('');
  let otp = $state('');
  let name = $state('');
  let needCode = $state(false);
  let otpEl = $state();
  // The authenticator code is what's asked next: put the cursor there.
  $effect(() => {
    if (needCode && otpEl) otpEl.focus();
  });

  const setup = $derived(hello.setup_needed);
  // Opened by Hopper Server on this PC (tray, first-run dialog): the setup
  // code comes in the link. Taken out of the address bar right away.
  const linked = (location.hash.match(/[#&]setup=([A-Za-z0-9]+)/) || [])[1] || '';
  if (linked) {
    code = linked;
    history.replaceState(null, '', location.pathname + location.search);
  }
  // A browser on the server PC itself needs no setup code.
  const noCode = $derived(setup && (!!linked || hello.local_setup));

  async function submit(e) {
    e.preventDefault();
    error = '';
    if (setup && password !== confirm) {
      error = "The passwords don't match.";
      return;
    }
    busy = true;
    try {
      if (setup) await post('/auth/setup', { code, password });
      else await post('/auth/login', { name, password, code: needCode ? otp : null });
      onDone();
    } catch (err) {
      if (err.data?.need_code) {
        needCode = true;
        error = otp ? err.message : '';
      } else {
        error = err.message;
      }
    } finally {
      busy = false;
    }
  }
</script>

<div class="wrap">
  <form class="card box" onsubmit={submit}>
    <div class="brand">
      <img class="logo" src="/logo.svg" alt="" />
      <div>
        <div class="title">Hopper Server</div>
        <div class="small muted">{hello.name} · v{hello.version}</div>
      </div>
    </div>
    {#if noCode}
      <span class="pill running ok"><CircleCheck size={13} />{linked ? 'Opened from Hopper Server on this PC' : 'On this PC, no setup code needed'}</span>
      <p class="small subtle">Pick the owner password. You'll use it to sign in from any device.</p>
    {:else if setup}
      <p class="small subtle">
        Set the owner password. Easiest: open <span class="mono">http://localhost:{hello.port}</span> on the server PC
        itself. From here you need the setup code: it's in <span class="mono">setup-code.txt</span> in the panel's data
        folder, and in its log.
      </p>
      <label class="lbl" for="code">Setup code</label>
      <div class="input">
        <KeyRound size={14} /><input id="code" bind:value={code} autocomplete="off" spellcheck="false" placeholder="8 characters" />
      </div>
    {/if}
    {#if !setup}
      <label class="lbl" for="nm">Name <span class="muted">(empty for the owner)</span></label>
      <div class="input"><User size={14} /><input id="nm" bind:value={name} autocomplete="username" spellcheck="false" placeholder="owner" /></div>
    {/if}
    <label class="lbl" for="pw">{setup ? 'New password' : 'Password'}</label>
    <div class="input">
      <Lock size={14} />
      <input id="pw" type="password" bind:value={password} autocomplete={setup ? 'new-password' : 'current-password'} />
    </div>
    {#if setup}
      <label class="lbl" for="pw2">Repeat it</label>
      <div class="input"><Lock size={14} /><input id="pw2" type="password" bind:value={confirm} autocomplete="new-password" /></div>
    {/if}
    {#if needCode}
      <label class="lbl" for="otp">Code from your authenticator app</label>
      <div class="input"><KeyRound size={14} /><input id="otp" bind:this={otpEl} bind:value={otp} inputmode="numeric" autocomplete="one-time-code" maxlength="6" placeholder="123456" /></div>
    {/if}
    {#if location.protocol === 'http:' && !['localhost', '127.0.0.1'].includes(location.hostname)}
      <p class="tiny muted">Not encrypted. Fine on your own LAN; from elsewhere use https://{location.host}.</p>
    {/if}
    {#if error}<p class="err">{error}</p>{/if}
    <button class="btn pri big" disabled={busy || !password || (setup && !code && !noCode)}>
      {setup ? 'Set up the panel' : 'Sign in'}
    </button>
  </form>
</div>

<style>
  .wrap { min-height: 100%; display: grid; place-items: center; padding: 16px; }
  .box { width: min(380px, 100%); display: flex; flex-direction: column; gap: 10px; padding: 22px; }
  .brand { display: flex; align-items: center; gap: 11px; margin-bottom: 6px; }
  .title { font-weight: 700; font-size: 16px; }
  .logo { width: 34px; height: 34px; image-rendering: pixelated; flex: none; }
  label { margin-top: 4px; }
  .ok { align-self: flex-start; height: 24px; }
  .big { justify-content: center; padding: 9px; margin-top: 6px; font-size: 13px; }
</style>
