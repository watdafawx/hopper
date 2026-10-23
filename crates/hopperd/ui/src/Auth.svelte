<script>
  import { Server, KeyRound, Lock } from '@lucide/svelte';
  import { post } from './lib/api.js';

  let { hello, onDone } = $props();
  let code = $state('');
  let password = $state('');
  let confirm = $state('');
  let busy = $state(false);
  let error = $state('');
  let otp = $state('');
  let needCode = $state(false);

  const setup = $derived(hello.setup_needed);

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
      else await post('/auth/login', { password, code: needCode ? otp : null });
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
      <div class="logo"><Server size={18} strokeWidth={2.5} /></div>
      <div>
        <div class="title">Hopper Server</div>
        <div class="small muted">{hello.name} · v{hello.version}</div>
      </div>
    </div>
    {#if setup}
      <p class="small subtle">
        Set the owner password. The setup code is in hopperd's log and in <span class="mono">setup-code.txt</span> in
        its data folder.
      </p>
      <label class="lbl" for="code">Setup code</label>
      <div class="input">
        <KeyRound size={14} /><input id="code" bind:value={code} autocomplete="off" spellcheck="false" placeholder="8 characters" />
      </div>
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
      <div class="input"><KeyRound size={14} /><input id="otp" bind:value={otp} inputmode="numeric" autocomplete="one-time-code" maxlength="6" placeholder="123456" /></div>
    {/if}
    {#if location.protocol === 'http:' && !['localhost', '127.0.0.1'].includes(location.hostname)}
      <p class="tiny muted">Not encrypted. Fine on your own LAN; from elsewhere use https://{location.host}.</p>
    {/if}
    {#if error}<p class="err">{error}</p>{/if}
    <button class="btn pri big" disabled={busy || !password || (setup && !code)}>
      {setup ? 'Set up the panel' : 'Sign in'}
    </button>
  </form>
</div>

<style>
  .wrap { min-height: 100%; display: grid; place-items: center; padding: 16px; }
  .box { width: min(380px, 100%); display: flex; flex-direction: column; gap: 10px; padding: 22px; }
  .brand { display: flex; align-items: center; gap: 11px; margin-bottom: 6px; }
  .title { font-weight: 700; font-size: 16px; }
  .logo { width: 34px; height: 34px; border-radius: 9px; background: linear-gradient(135deg, #fb923c, #ea580c); display: grid; place-items: center; color: #1a1a1f; }
  label { margin-top: 4px; }
  .big { justify-content: center; padding: 9px; margin-top: 6px; font-size: 13px; }
</style>
