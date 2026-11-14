<script>
  // "Relay through a VPS" setup (design/vps-relay.md, B3; option A of
  // design/mocks/vps-relay.html): one dialog, three steps. Connect signs in
  // to the VPS over ssh, Check reads what it is and whether the ports are
  // free, Install runs the steps on it. Each install step checks before it
  // acts, so the same list is the repair view later ("Run again"), and
  // Remove takes out what it put there. Owner only, like the API.
  import { tick } from 'svelte';
  import { Check, X, Copy, LoaderCircle, Circle, TriangleAlert, Minus, KeyRound } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { copyText } from './lib/browse.js';
  import { confirmTick, confirmIt } from './lib/dialog.svelte.js';

  let { server, onClose } = $props();

  let st = $state(null);
  let stage = $state('');
  let busy = $state('');
  let err = $state('');
  let showCmds = $state(false);

  // Connect form.
  let host = $state('');
  let sshPort = $state(22);
  let user = $state('');
  let mode = $state('key');
  let keyText = $state('');
  let keyName = $state('');
  let passphrase = $state('');
  let password = $state('');
  let trustNew = $state(false);
  let keyChanged = $state(false);
  let filePick = $state();
  let field = $state();

  // Check and install.
  let gamePort = $state(25565);
  let tunnelPort = $state(25566);
  let publicAddress = $state('');

  const H = { home: true };
  const login = $derived(st?.login);
  const check = $derived(st?.check);
  const run = $derived(st?.run);
  const running = $derived(!!run?.running);

  async function load() {
    try {
      st = await api('/vps-relay/setup', { ...H, poll: true });
    } catch (e) {
      err = e.message;
    }
  }

  // Where the dialog opens: a run in progress or just ended, a saved login
  // to check, or the connect form.
  load().then(() => {
    if (!st) return;
    const l = st.login;
    if (l) {
      host = l.host;
      sshPort = l.port || 22;
      user = l.user;
      mode = 'saved';
    }
    gamePort = st.check?.ports?.game ?? st.ports?.game ?? 25565;
    tunnelPort = st.check?.ports?.tunnel ?? st.ports?.tunnel ?? 25566;
    publicAddress = st.paired?.public_address || '';
    stage = st.run ? 'run' : l ? 'check' : 'connect';
    if (stage === 'check' && !st.check) runCheck();
    if (stage === 'connect') tick().then(() => field?.focus());
  });

  // Follows a run while it goes.
  $effect(() => {
    if (!running) return;
    const t = setInterval(load, 700);
    return () => clearInterval(t);
  });

  async function act(what, fn) {
    if (busy) return;
    busy = what;
    err = '';
    try {
      await fn();
    } catch (e) {
      err = e.message;
    } finally {
      busy = '';
    }
  }

  async function pickFile(e) {
    const f = e.currentTarget.files?.[0];
    e.currentTarget.value = '';
    if (!f) return;
    // A private key is a few KB; anything big is the wrong file.
    if (f.size > 64 * 1024) {
      err = "That file is too big to be an ssh key. Pick the private key (like id_ed25519), not the .pub one.";
      return;
    }
    keyText = await f.text();
    keyName = f.name;
  }

  const newKey = () =>
    act('newkey', async () => {
      const r = await post('/vps-relay/setup/new-key', {}, H);
      st = { ...st, new_public: r.public_key };
    });

  const connect = (e) => {
    e?.preventDefault();
    return act('connect', async () => {
      const body = { host: host.trim(), port: Number(sshPort) || 22, user: user.trim(), mode: mode === 'saved' ? 'key' : mode, trust_new: trustNew };
      if (mode === 'key') Object.assign(body, { key: keyText, passphrase });
      if (mode === 'password') body.password = password;
      if (mode === 'saved') throw new Error('Pick how to sign in.');
      try {
        await post('/vps-relay/setup/connect', body, { ...H, timeout: 90000 });
      } catch (e) {
        keyChanged = /host key changed/i.test(e.message);
        throw e;
      }
      // Nothing secret stays in the page once it's saved on the panel.
      password = '';
      keyText = '';
      passphrase = '';
      trustNew = false;
      keyChanged = false;
      await load();
      stage = 'check';
      await doCheck();
    });
  };

  async function doCheck() {
    const r = await post('/vps-relay/setup/check', { game_port: Number(gamePort), tunnel_port: Number(tunnelPort) }, { ...H, timeout: 120000 });
    st = { ...st, check: r };
  }
  const runCheck = () => act('check', doCheck);

  const install = () =>
    act('install', async () => {
      st = await post('/vps-relay/setup/install', { server: server.id, game_port: Number(gamePort), tunnel_port: Number(tunnelPort), public_address: publicAddress.trim() }, H);
      stage = 'run';
      showCmds = false;
    });

  async function remove() {
    const ok = await confirmTick('Remove the relay from the VPS?', {
      body: `Stops the relay on ${login?.host}, deletes its program, keys and user, takes out the firewall rules it added and disconnects this panel. Players can't join through the VPS any more. Nothing else on the VPS changes.`,
      check: 'Yes, remove it',
      ok: 'Remove the relay',
      danger: true,
    });
    if (!ok?.ticked) return;
    await act('remove', async () => {
      st = await post('/vps-relay/setup/remove', {}, H);
      stage = 'run';
      showCmds = false;
    });
  }

  async function forget() {
    // Nothing on the VPS changes: say so before the login is gone.
    if (st?.paired?.address && !(await confirmIt('Forget this VPS?', { body: `Disconnects this panel and forgets the login for ${login?.host}, without connecting to it. Whatever the setup put on the VPS stays there; use Remove the relay to take it off.`, ok: 'Forget it' }))) return;
    await act('forget', async () => {
      st = await post('/vps-relay/setup/forget', {}, H);
      mode = 'key';
      stage = 'connect';
    });
  }

  function reconnect() {
    err = '';
    if (mode === 'saved') mode = 'key';
    stage = 'connect';
    tick().then(() => field?.focus());
  }

  // Back to Check from a finished run (Run again).
  function again() {
    err = '';
    stage = 'check';
    runCheck();
  }

  async function copy(text, what) {
    say((await copyText(text)) ? `Copied ${what}` : `Couldn't copy ${what}`, false);
  }

  const portsRow = $derived(check?.rows?.find((r) => r.id === 'ports'));
  const okToInstall = $derived(!!check?.ok && Number(gamePort) === check?.ports?.game && Number(tunnelPort) === check?.ports?.tunnel);
  const remove_ = $derived(run?.kind === 'remove');
  const doneSteps = $derived(run ? run.steps.filter((s) => !['wait', 'run'].includes(s.state)).length : 0);
  const pct = $derived(run?.steps?.length ? Math.round((doneSteps / run.steps.length) * 100) : 0);
  const failed = $derived(!!run && !run.running && !!run.error);
  const finished = $derived(!!run && !run.running && !run.error);
  const joinAt = $derived(st?.paired?.public_address || (login ? `${login.host}${check?.ports?.game && check.ports.game !== 25565 ? `:${check.ports.game}` : ''}` : ''));
  const commands = $derived(stage === 'run' ? run?.commands ?? [] : check?.commands ?? []);

  // The stepper: what is done, what is open.
  const at = $derived(stage === 'connect' ? 1 : stage === 'check' ? 2 : 3);
  const third = $derived(stage === 'run' && remove_ ? 'Remove' : 'Install');

  const canConnect = $derived(host.trim() && user.trim() && (mode === 'key' ? keyText.trim() : mode === 'password' ? password : mode === 'new' ? !!st?.new_public : false));

  function keys(e) {
    if (e.key === 'Escape' && !busy) {
      e.preventDefault();
      onClose();
    }
  }
</script>

<svelte:window onkeydown={keys} />

{#snippet mark(state)}
  <span class="st {state}">
    {#if state === 'ok' || state === 'done'}<Check size={13} />
    {:else if state === 'fail'}<X size={13} />
    {:else if state === 'warn'}<TriangleAlert size={12} />
    {:else if state === 'run'}<LoaderCircle size={13} class="spin" />
    {:else if state === 'skip'}<Minus size={13} />
    {:else}<Circle size={9} />{/if}
  </span>
{/snippet}

{#snippet hostKey()}
  {#if login?.host_key}
    <div class="hk tiny muted" title="The VPS's ssh host key. Hopper checks it on every connection, so nothing else can pose as this VPS."><KeyRound size={12} /><span class="nw">Host key</span><span class="mono">{login.host_key}</span></div>
  {/if}
{/snippet}

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onClose()}>
  <div class="box popover" role="dialog" aria-modal="true" aria-labelledby="vr-title">
    <div class="hd">
      <h3 id="vr-title">Relay through a VPS</h3>
      <span class="sp"></span>
      <button class="btn ghost sm icon" title="Close" aria-label="Close" disabled={!!busy} onclick={onClose}><X size={14} /></button>
    </div>

    <div class="stepper" aria-label="Steps">
      <span class:on={at === 1} class:done={at > 1}><i>{#if at > 1}<Check size={10} strokeWidth={3} />{:else}1{/if}</i>Connect</span>
      <span class:on={at === 2} class:done={at > 2}><i>{#if at > 2}<Check size={10} strokeWidth={3} />{:else}2{/if}</i>Check</span>
      <span class:on={at === 3 && !finished} class:done={finished}><i>{#if finished}<Check size={10} strokeWidth={3} />{:else}3{/if}</i>{third}</span>
    </div>

    {#if !st && !err}
      <div class="loading tiny muted"><LoaderCircle size={13} class="spin" />Loading…</div>
    {:else if stage === 'connect'}
      <form class="f" onsubmit={connect}>
        <p class="small subtle">Players join the VPS; this panel sends the game to it over one connection it opens itself, so nothing needs opening at home. Hopper signs in to the VPS over ssh to set it up.</p>
        <div class="frow">
          <label class="small" for="vr-host">VPS address</label>
          <div class="pair">
            <div class="input grow"><input id="vr-host" bind:this={field} bind:value={host} placeholder="203.0.113.10" spellcheck="false" autocomplete="off" /></div>
            <div class="input port" title="The VPS's ssh port"><span class="tiny">port</span><input bind:value={sshPort} inputmode="numeric" aria-label="ssh port" /></div>
          </div>
          <label class="small" for="vr-user">ssh user</label>
          <div class="input"><input id="vr-user" bind:value={user} placeholder="ubuntu, debian or root" spellcheck="false" autocomplete="off" /></div>
          <span class="small">Sign in with</span>
          <div class="seg" role="radiogroup" aria-label="Sign in with">
            {#if login}<button type="button" role="radio" aria-checked={mode === 'saved'} class:on={mode === 'saved'} onclick={() => (mode = 'saved')}>Saved key</button>{/if}
            <button type="button" role="radio" aria-checked={mode === 'key'} class:on={mode === 'key'} onclick={() => (mode = 'key')}>Key file</button>
            <button type="button" role="radio" aria-checked={mode === 'password'} class:on={mode === 'password'} onclick={() => (mode = 'password')}>Password</button>
            <button type="button" role="radio" aria-checked={mode === 'new'} class:on={mode === 'new'} onclick={() => ((mode = 'new'), st?.new_public || newKey())}>New key</button>
          </div>
          {#if mode === 'key'}
            <span class="small top">Private key</span>
            <div class="col">
              <textarea class="keybox mono" bind:value={keyText} rows="3" placeholder="Paste it (-----BEGIN OPENSSH PRIVATE KEY----- …) or choose the file" spellcheck="false" autocomplete="off"></textarea>
              <div class="row">
                <button type="button" class="btn sm" onclick={() => filePick.click()}>Choose file…</button>
                {#if keyName}<span class="tiny muted ell">{keyName}</span>{/if}
                <input type="file" hidden bind:this={filePick} onchange={pickFile} />
              </div>
            </div>
            <label class="small" for="vr-phrase">Passphrase</label>
            <div class="input"><input id="vr-phrase" type="password" bind:value={passphrase} placeholder="Only if the key has one" autocomplete="off" /></div>
          {:else if mode === 'password'}
            <label class="small" for="vr-pass">Password</label>
            <div class="input"><input id="vr-pass" type="password" bind:value={password} autocomplete="off" /></div>
          {:else if mode === 'new'}
            <span class="small top">Public key</span>
            <div class="col">
              {#if st?.new_public}
                <div class="pub"><span class="mono">{st.new_public}</span><button type="button" class="btn sm" onclick={() => copy(st.new_public, 'the public key')}><Copy size={13} />Copy</button></div>
                <span class="tiny muted">Add it to the VPS first: your cloud's ssh keys page, or a line in <span class="mono">~/.ssh/authorized_keys</span>. Then press Connect.</span>
              {:else}
                <span class="tiny muted"><LoaderCircle size={12} class="spin" /> Making a key…</span>
              {/if}
            </div>
          {:else if mode === 'saved'}
            <span></span>
            <span class="tiny muted">The key Hopper saved for {login.user}@{login.host}. To change it, pick another way.</span>
          {/if}
        </div>
        {#if keyChanged}
          <label class="tick small"><input type="checkbox" bind:checked={trustNew} />I reinstalled the VPS: trust its new host key</label>
        {/if}
        {@render hostKey()}
        <div class="note">
          {#if mode === 'password'}The password is used once, to put a new key on the VPS, and isn't kept. Hopper keeps that key for repair and removal.{:else}Hopper keeps the key on this panel, readable by the owner only, to repair or remove the relay later. The user needs sudo.{/if}
        </div>
        {#if err}<div class="fix" role="alert">{err}</div>{/if}
        <div class="row end">
          {#if login}<button type="button" class="btn ghost" disabled={!!busy} onclick={() => ((stage = 'check'), (mode = 'saved'))}>Back</button>{/if}
          <span class="sp"></span>
          <button type="button" class="btn" disabled={!!busy} onclick={onClose}>Cancel</button>
          <button class="btn pri" disabled={!!busy || !canConnect}>{#if busy === 'connect'}<LoaderCircle size={13} class="spin" />Connecting…{:else}Connect{/if}</button>
        </div>
      </form>
    {:else if stage === 'check'}
      <div class="f">
        <div class="who small"><span>Signed in to <b class="mono">{login?.user}@{login?.host}</b>{login?.port && login.port !== 22 ? ` (port ${login.port})` : ''}</span><button class="btn ghost sm" disabled={!!busy} onclick={reconnect}>Change</button></div>
        {@render hostKey()}
        {#if busy === 'check' && !check}
          <div class="loading tiny muted"><LoaderCircle size={13} class="spin" />Checking the VPS…</div>
        {:else if check}
          <ul class="steps" class:stale={busy === 'check'}>
            {#each check.rows as r (r.id)}
              <li class:bad={r.state === 'fail'}>{@render mark(r.state)}<span class="tx">{r.text}</span></li>
            {/each}
          </ul>
        {/if}
        <div class="frow">
          <label class="small" for="vr-game">Players' port</label>
          <div class="pair">
            <div class="input num" class:busy={portsRow?.busy?.includes(Number(gamePort))}><input id="vr-game" bind:value={gamePort} inputmode="numeric" /></div>
            <label class="small lab2" for="vr-tun">Tunnel port</label>
            <div class="input num" class:busy={portsRow?.busy?.includes(Number(tunnelPort))}><input id="vr-tun" bind:value={tunnelPort} inputmode="numeric" /></div>
          </div>
          <label class="small" for="vr-pub">Players join at</label>
          <div class="input"><input id="vr-pub" bind:value={publicAddress} placeholder={login ? `${login.host} (optional: a name like play.example.com)` : ''} spellcheck="false" autocomplete="off" /></div>
        </div>
        <div class="tiny muted">Installs Hopper Server {st.version} in relay mode on the VPS, as its own service and user, and opens the two ports in its firewall. {st.source === 'file' ? 'Test build: the program comes from this PC.' : 'The program comes from the Hopper Server release on GitHub, checked against its checksum.'}</div>
        {#if err}<div class="fix" role="alert">{err}</div>{/if}
        {#if commands.length}<button class="linkish tiny" onclick={() => (showCmds = !showCmds)}>{showCmds ? 'Hide the commands' : 'Show the commands'}</button>{/if}
        {#if showCmds}<pre class="cmds mono">{commands.join('\n')}</pre>{/if}
        <div class="row end">
          <!-- Set up before (paired, or something was installed): it can come off from here too. -->
          {#if login && st.paired?.address}<button class="btn ghost" disabled={!!busy} onclick={remove}>Remove the relay…</button>{/if}
          {#if login}<button class="btn ghost" disabled={!!busy} onclick={forget} title="Forget the VPS here without connecting to it (it was deleted)">Forget this VPS</button>{/if}
          <span class="sp"></span>
          <button class="btn" disabled={!!busy} onclick={runCheck}>{#if busy === 'check'}<LoaderCircle size={13} class="spin" />Checking…{:else}Check again{/if}</button>
          <button class="btn pri" disabled={!!busy || !okToInstall} title={okToInstall ? '' : check?.ok ? 'The ports changed: check again first' : 'Fix what the check found first'} onclick={install}>{#if busy === 'install'}<LoaderCircle size={13} class="spin" />Starting…{:else}Install{/if}</button>
        </div>
      </div>
    {:else if stage === 'run' && run}
      <div class="f">
        <div class="who small"><span>{remove_ ? 'Removing the relay from' : 'Setting up'} <b class="mono">{login?.user ? `${login.user}@` : ''}{login?.host || 'the VPS'}</b></span></div>
        <ul class="steps run">
          {#each run.steps as s (s.id)}
            <li class:bad={s.state === 'fail'} class:wait={s.state === 'wait' || s.state === 'skip'} class:long={s.state === 'warn' || s.state === 'fail'}>{@render mark(s.state)}<span class="tx">{s.title}</span>{#if s.detail}<span class="d" title={s.detail}>{s.detail}</span>{/if}</li>
          {/each}
        </ul>
        <div class="bar" class:g={finished}><i style="width: {pct}%"></i></div>
        {#if failed}
          <div class="fix" role="alert">{run.error}</div>
        {:else if finished && remove_}
          <div class="note good">The relay is off the VPS: no relay user, service, files or firewall rules are left, and this panel is disconnected.</div>
        {:else if finished && run.changed === 0}
          <div class="note good">Everything was already right. Nothing changed.</div>
        {:else if finished}
          <div class="note good">The relay is running. Players join <span class="mono">{joinAt}</span>; the server's Overview shows the connection.</div>
        {:else}
          <div class="tiny muted">You can close this: it keeps going, and the panel's task list shows how far it got.</div>
        {/if}
        {#if err}<div class="fix" role="alert">{err}</div>{/if}
        {#if commands.length}<button class="linkish tiny" onclick={() => (showCmds = !showCmds)}>{showCmds ? 'Hide the commands' : 'Show the commands'}</button>{/if}
        {#if showCmds}<pre class="cmds mono">{commands.join('\n')}</pre>{/if}
        <div class="row end">
          {#if !running && !remove_ && login}<button class="btn ghost" disabled={!!busy} onclick={remove}>Remove the relay…</button>{/if}
          {#if !running && remove_ && failed && login}<button class="btn ghost" disabled={!!busy} onclick={remove}>Try again</button>{/if}
          <span class="sp"></span>
          {#if !running && !remove_ && login}<button class="btn" disabled={!!busy} onclick={again}>Run again</button>{/if}
          {#if !running && remove_ && !login}<button class="btn" disabled={!!busy} onclick={() => ((stage = 'connect'), (mode = 'key'))}>Set up again</button>{/if}
          <button class="btn" class:pri={!running} onclick={onClose}>{running ? 'Close' : 'Done'}</button>
        </div>
      </div>
    {:else}
      {#if err}<div class="fix" role="alert">{err}</div>{/if}
      <div class="row end"><button class="btn" onclick={onClose}>Close</button></div>
    {/if}
  </div>
</div>

<style>
  .dialog-back { z-index: 80; }
  .box { width: min(560px, 100%); padding: 16px 18px 18px; display: flex; flex-direction: column; gap: 12px; }
  .hd { display: flex; align-items: center; gap: 8px; }
  .hd h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .sp { flex: 1; }
  .f { display: flex; flex-direction: column; gap: 12px; }
  .f p { margin: 0; }
  .stepper { display: flex; gap: 14px; font-size: 11.5px; color: var(--muted); }
  .stepper span { display: inline-flex; align-items: center; gap: 6px; }
  .stepper i { width: 16px; height: 16px; border-radius: 50%; display: grid; place-items: center; font-style: normal; font-size: 10px; font-weight: 700; background: var(--raise); color: var(--muted); flex: none; }
  .stepper span.on { color: var(--text); }
  .stepper span.on i { background: var(--acc); color: #1c0d02; }
  .stepper span.done { color: var(--subtle); }
  .stepper span.done i { background: var(--green-dim); color: var(--green); }
  .frow { display: grid; grid-template-columns: 104px minmax(0, 1fr); align-items: center; gap: 10px; }
  .frow .top { align-self: start; padding-top: 7px; }
  .pair { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .grow { flex: 1; min-width: 0; }
  .port { width: 92px; flex: none; }
  .port input { width: 100%; }
  .num { width: 84px; flex: none; }
  .num.busy { border-color: var(--red); }
  .lab2 { margin-left: 8px; white-space: nowrap; }
  .col { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
  .row { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .keybox { width: 100%; box-sizing: border-box; resize: vertical; min-height: 58px; background: var(--base); border: 1px solid var(--input-line); border-radius: var(--radius); padding: 7px 10px; color: var(--text); font-size: 11.5px; }
  .keybox:focus { border-color: var(--acc); outline: 0; }
  .pub { display: flex; align-items: center; gap: 8px; background: var(--base); border: 1px solid var(--line2); border-radius: var(--radius); padding: 5px 5px 5px 10px; min-width: 0; }
  .pub .mono { font-size: 11.5px; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .seg { display: inline-flex; width: fit-content; border: 1px solid var(--line2); border-radius: var(--radius); overflow: hidden; flex-wrap: wrap; }
  /* At an input's height, like every field in the form. */
  .seg button { box-sizing: border-box; height: 32px; padding: 0 12px; font-size: 12.5px; color: var(--subtle); border-left: 1px solid var(--line2); }
  .seg button:first-child { border-left: 0; }
  .seg button:hover { background: var(--hover); }
  .seg button.on { background: var(--acc-dim); color: var(--text); font-weight: 500; }
  .seg button:focus-visible { outline: 2px solid var(--acc); outline-offset: -2px; }
  .tick { display: flex; align-items: center; gap: 8px; }
  .hk { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .hk .nw { white-space: nowrap; flex: none; }
  .hk .mono { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .who { display: flex; align-items: center; gap: 8px; justify-content: space-between; min-width: 0; }
  .who > span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .who b { font-weight: 600; }
  .loading { display: flex; align-items: center; gap: 7px; padding: 6px 0; }
  .steps { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 2px; font-size: 12.5px; }
  .steps.stale { opacity: .55; }
  .steps li { display: flex; align-items: center; gap: 8px; padding: 4px 6px; border-radius: var(--radius); min-width: 0; }
  .steps li:hover { background: var(--hover); }
  .steps li.bad { color: var(--text); }
  .steps li.wait { color: var(--muted); }
  .steps .tx { min-width: 0; }
  .steps.run .tx { flex: none; }
  .steps .d { margin-left: auto; padding-left: 12px; color: var(--muted); font-size: 11.5px; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  /* A warning or a failure says what to do: on its own line, never cut off. */
  .steps li.long { flex-wrap: wrap; row-gap: 2px; }
  .steps li.long .d { flex-basis: 100%; margin-left: 26px; padding-left: 0; white-space: normal; overflow-wrap: anywhere; color: var(--subtle); font-size: 12px; }
  .st { width: 18px; height: 18px; display: grid; place-items: center; flex: none; border-radius: 50%; color: var(--muted); }
  .st.ok, .st.done { background: var(--green-dim); color: var(--green); }
  .st.fail { background: var(--red-dim); color: var(--red); }
  .st.warn { background: var(--gold-dim); color: var(--gold); }
  .st.run { color: var(--acc); }
  .note { font-size: 12px; color: var(--subtle); background: var(--hover); border-radius: var(--radius); padding: 8px 10px; }
  .note.good { background: var(--green-dim); color: var(--text); }
  .fix { font-size: 12.5px; border-radius: var(--radius); background: var(--red-dim); color: var(--text); padding: 8px 10px; overflow-wrap: anywhere; }
  .linkish { width: fit-content; color: var(--subtle); text-decoration: underline; text-underline-offset: 2px; padding: 0; }
  .linkish:hover { color: var(--text); }
  .cmds { margin: 0; max-height: 180px; overflow: auto; font-size: 11px; line-height: 1.5; background: var(--base); border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 10px; white-space: pre-wrap; overflow-wrap: anywhere; color: var(--subtle); }
  .end { justify-content: flex-end; flex-wrap: wrap; }
  :global(.spin) { animation: spin .8s linear infinite; }
  @media (max-width: 520px) {
    .frow { grid-template-columns: 1fr; gap: 4px; }
    .frow .top { padding-top: 0; }
    .pair { flex-wrap: wrap; }
    .lab2 { margin-left: 0; }
  }
</style>
