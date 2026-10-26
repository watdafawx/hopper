<script>
  // Places backups are copied to (a folder, SFTP, S3 / B2 / R2, another
  // Hopper Server), like Webhooks: a list, an inline form, Test, a switch.
  // Passwords and keys never come back from the panel once saved.
  import { onMount } from 'svelte';
  import { CloudUpload, Plus, Trash2, Pencil, PlugZap, AlertTriangle, KeyRound, Copy, Inbox } from '@lucide/svelte';
  import { api, post, del } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt, ask } from './lib/dialog.svelte.js';

  const KINDS = [
    { id: 'folder', label: 'Folder' },
    { id: 'sftp', label: 'SFTP' },
    { id: 's3', label: 'S3 / B2 / R2' },
    { id: 'hopper', label: 'Another Hopper Server' },
  ];
  const PRESETS = {
    aws: { endpoint: 's3.us-east-1.amazonaws.com', region: 'us-east-1' },
    b2: { endpoint: 's3.us-west-004.backblazeb2.com', region: '' },
    r2: { endpoint: '<account id>.r2.cloudflarestorage.com', region: 'auto' },
    other: { endpoint: '', region: '' },
  };

  let targets = $state([]);
  let receive = $state([]);
  let keySet = $state(false);
  let form = $state(null);
  let testing = $state(false);
  let shownKey = $state(null);
  let newReceive = $state(null);

  async function load() {
    try {
      const r = await api('/backup-targets');
      targets = r.targets;
      receive = r.receive;
      keySet = r.key_set;
    } catch (e) {
      say(e.message, true);
    }
  }

  function edit(t) {
    form = t
      ? { ...t, secret: '', replace: !t.secret_set }
      : { id: '', name: '', kind: 'folder', enabled: true, path: '', host: '', port: 22, user: '', dir: '', host_key: '', endpoint: '', region: '', bucket: '', prefix: '', access_key: '', url: '', secret: '', replace: true, secret_set: false, preset: 'b2' };
  }

  function preset(p) {
    form.preset = p;
    Object.assign(form, PRESETS[p]);
  }

  const body = () => {
    const { replace, preset: _p, secret_set: _s, servers: _v, place: _pl, ...t } = form;
    return { ...t, port: Number(t.port) || 0, secret: replace ? t.secret : '' };
  };

  async function test() {
    testing = true;
    try {
      const r = await post('/backup-targets/test', body());
      if (r.host_key && form.kind === 'sftp') form.host_key = r.host_key;
      say(r.host_key ? `It works. The SFTP server's key (${r.host_key}) is remembered.` : 'It works: written, read back and cleaned up.');
    } catch (e) {
      say(e.message, true);
    } finally {
      testing = false;
    }
  }

  async function save(e) {
    e.preventDefault();
    try {
      if (form.id) {
        await api(`/backup-targets/${form.id}`, { method: 'PUT', body: body() });
      } else {
        const r = await post('/backup-targets', body());
        if (r.recovery_key) shownKey = r.recovery_key;
      }
      form = null;
      load();
    } catch (err) {
      say(err.message, true);
    }
  }

  async function toggle(t) {
    try {
      await api(`/backup-targets/${t.id}`, { method: 'PUT', body: { ...t, enabled: !t.enabled, secret: '' } });
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function remove(t) {
    if (!(await confirmIt(`Remove ${t.name}?`, { body: 'Backups stop being copied there. What is already there stays; delete it there yourself if you want.', ok: 'Remove', danger: true }))) return;
    try {
      await del(`/backup-targets/${t.id}`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function importKey() {
    const k = await ask("Use another panel's backup key", '', { body: "To bring back backups another panel copied (for example after reinstalling). Only before this panel copies anything with its own key.", ok: 'Use it', placeholder: '64 letters and digits' });
    if (!k) return;
    try {
      await post('/backup-targets/key', { key: k });
      say('Key saved.');
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function addReceive() {
    const name = await ask('Let another panel copy its backups here', '', { body: 'Makes a key to paste into that panel (Backup copies › Add › Another Hopper Server). Its copies go into this panel\'s data folder, encrypted.', ok: 'Make a key', placeholder: 'Name, e.g. rig' });
    if (name === null) return;
    try {
      newReceive = await post('/backup-receive', { name });
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function removeReceive(r) {
    if (!(await confirmIt(`Stop taking copies from ${r.name}?`, { body: 'Its key stops working. What it already sent stays in the data folder (received-backups).', ok: 'Stop', danger: true }))) return;
    try {
      await del(`/backup-receive/${r.id}`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function copy(text) {
    try {
      await navigator.clipboard.writeText(text);
      say('Copied');
    } catch {
      say("Couldn't copy; select it instead.", true);
    }
  }

  const ago = (t) => (t ? new Date(t * 1000).toLocaleString() : 'not yet');
  const label = (k) => KINDS.find((x) => x.id === k)?.label || k;

  onMount(load);
</script>

<div class="card">
  <h3><CloudUpload size={14} />Backup copies <span class="sp"></span>
    {#if !form}<button class="btn sm" onclick={() => edit(null)}><Plus size={13} />Add</button>{/if}</h3>

  {#if shownKey}
    <div class="keybox">
      <div class="row"><KeyRound size={14} /><b>Save this backup key somewhere safe</b></div>
      <p class="small subtle">Copies are encrypted with it. If this PC is lost, a new panel needs it to open them. It is shown only now.</p>
      <div class="row"><span class="mono key grow">{shownKey}</span><button class="btn sm" onclick={() => copy(shownKey)}><Copy size={13} />Copy</button></div>
      <div class="row end"><button class="btn sm pri" onclick={() => (shownKey = null)}>I saved it</button></div>
    </div>
  {/if}

  {#if form}
    <form class="col" onsubmit={save}>
      <div class="chips">
        {#each KINDS as k (k.id)}
          <button type="button" class="chip" class:acc={form.kind === k.id} disabled={!!form.id} onclick={() => (form.kind = k.id)}>{k.label}</button>
        {/each}
      </div>
      {#if form.kind === 'folder'}
        <label class="f"><span>Folder</span><div class="input grow"><input bind:value={form.path} placeholder="D:\Backups or \\nas\backups" required /></div></label>
        <div class="tiny muted f0">A drive, USB disk or network share this PC can write to.</div>
      {:else if form.kind === 'sftp'}
        <label class="f"><span>Host</span><div class="input grow"><input bind:value={form.host} placeholder="nas.local" required /></div><div class="input port"><input type="number" bind:value={form.port} min="1" max="65535" /></div></label>
        <label class="f"><span>User</span><div class="input grow"><input bind:value={form.user} autocomplete="off" required /></div></label>
        <label class="f"><span>Folder</span><div class="input grow"><input bind:value={form.dir} placeholder="backups/rig (empty: the user's home)" /></div></label>
        {#if form.host_key}<div class="tiny muted f0">Server key <span class="mono">{form.host_key}</span></div>{/if}
      {:else if form.kind === 's3'}
        {#if !form.id}
          <div class="f"><span>Provider</span><div class="seg">
            {#each [['aws', 'AWS'], ['b2', 'B2'], ['r2', 'R2'], ['other', 'Other']] as [p, l] (p)}<button type="button" class:on={form.preset === p} onclick={() => preset(p)}>{l}</button>{/each}
          </div></div>
        {/if}
        <label class="f"><span>Endpoint</span><div class="input grow"><input bind:value={form.endpoint} placeholder="s3.eu-central-003.backblazeb2.com" required /></div></label>
        <label class="f"><span>Bucket</span><div class="input grow"><input bind:value={form.bucket} required /></div><div class="input grow"><input bind:value={form.prefix} placeholder="Folder in it (optional)" /></div></label>
        <label class="f"><span>Region</span><div class="input grow"><input bind:value={form.region} placeholder="Worked out from the endpoint" /></div></label>
        <label class="f"><span>Key ID</span><div class="input grow"><input bind:value={form.access_key} autocomplete="off" required /></div></label>
      {:else}
        <label class="f"><span>Address</span><div class="input grow"><input bind:value={form.url} placeholder="http://192.168.50.7:8790" required /></div></label>
        <div class="tiny muted f0">On the other panel: Access &amp; devices › Backup copies › Receive copies › Make a key. Plain http works on the LAN and Tailscale.</div>
      {/if}
      {#if form.kind !== 'folder'}
        <div class="f"><span>{form.kind === 'sftp' ? 'Password' : form.kind === 's3' ? 'Secret key' : 'Receive key'}</span>
          {#if form.replace}
            <div class="input grow"><input type="password" bind:value={form.secret} autocomplete="new-password" required={!form.id} /></div>
          {:else}
            <span class="muted grow">Saved ••••••</span><button type="button" class="btn sm" onclick={() => (form.replace = true)}>Replace</button>
          {/if}
        </div>
      {/if}
      <label class="f"><span>Name</span><div class="input grow"><input bind:value={form.name} placeholder="Optional, e.g. NAS" /></div></label>
      <div class="tiny muted">Copies are encrypted on this PC first; the target only sees random names. Only what changed is sent.</div>
      <div class="row"><button type="button" class="btn sm" disabled={testing} onclick={test}><PlugZap size={13} />{testing ? 'Testing…' : 'Test'}</button>
        <span class="sp"></span>
        <button type="button" class="btn sm" onclick={() => (form = null)}>Cancel</button>
        <button class="btn sm pri">Save</button></div>
    </form>
  {:else}
    {#each targets as t (t.id)}
      {@const bad = t.servers.find((s) => s.last_error)}
      <div class="hook" class:off={!t.enabled}>
        <div class="grow">
          <div>{t.name} <span class="chip">{label(t.kind)}</span></div>
          <div class="tiny muted ell mono">{t.place}</div>
          <div class="tiny muted">{t.servers.length ? t.servers.map((s) => `${s.name}: ${s.copies} backup${s.copies === 1 ? '' : 's'}${s.pending ? `, ${s.pending} waiting` : ''}`).join(' · ') : 'No server copies here yet; pick it in a server\'s Backups tab.'}</div>
          {#if bad}<div class="tiny err"><AlertTriangle size={11} /> {bad.name}: {bad.last_error}</div>{/if}
        </div>
        <label class="sw" title={t.enabled ? 'Pause copies' : 'Resume copies'}><input type="checkbox" checked={t.enabled} onchange={() => toggle(t)} /><span></span></label>
        <button class="btn sm icon ghost" title="Edit" onclick={() => edit(t)}><Pencil size={13} /></button>
        <button class="btn sm icon ghost" title="Remove" onclick={() => remove(t)}><Trash2 size={13} /></button>
      </div>
    {:else}
      <p class="small subtle">Copy backups somewhere else too: a NAS or USB disk, an SFTP server, S3 / B2 / R2, or another Hopper Server. Then pick it in a server's Backups tab.
        {#if !keySet}<button class="lnk" onclick={importKey}>Use a key from another panel</button>{/if}</p>
    {/each}

    <div class="sub">
      <div class="row small"><Inbox size={13} /><b>Receive copies</b><span class="sp"></span><button class="btn sm ghost" onclick={addReceive}><Plus size={13} />Make a key</button></div>
      {#if newReceive}
        <div class="keybox">
          <div class="small">Paste this into the other panel. It's shown only now.</div>
          <div class="row"><span class="mono key grow">{newReceive.key}</span><button class="btn sm" onclick={() => copy(newReceive.key)}><Copy size={13} />Copy</button><button class="btn sm" onclick={() => (newReceive = null)}>Done</button></div>
        </div>
      {/if}
      {#each receive as r (r.id)}
        <div class="row small rcv"><span class="grow">{r.name} <span class="tiny muted">· since {ago(r.at)} · {r.files} files</span></span>
          <button class="btn sm icon ghost" title="Stop taking copies" onclick={() => removeReceive(r)}><Trash2 size={13} /></button></div>
      {:else}
        {#if !newReceive}<div class="tiny muted">Other Hopper Servers can copy their backups here with a key you make.</div>{/if}
      {/each}
    </div>
  {/if}
</div>

<style>
  .col { display: flex; flex-direction: column; gap: 8px; }
  .chips { display: flex; flex-wrap: wrap; gap: 5px; }
  .chips .chip { cursor: pointer; }
  .chip.acc { color: #fb923c; background: var(--acc-dim); }
  .f { display: flex; align-items: center; gap: 8px; font-size: 12.5px; }
  .f > span:first-child { width: 86px; flex: none; color: var(--muted); }
  .f0 { margin-left: 94px; }
  .port { width: 80px; }
  .grow { flex: 1; min-width: 0; }
  .seg { display: inline-flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; }
  .seg button { padding: 3px 10px; font-size: 12px; color: var(--subtle); }
  .seg button.on { background: var(--acc); color: #1a1a1f; }
  .hook { display: flex; align-items: center; gap: 8px; padding: 8px 0; border-bottom: 1px solid var(--line); }
  .hook.off { opacity: 0.55; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .err { color: var(--red); display: flex; align-items: center; gap: 4px; }
  .sw { position: relative; width: 30px; height: 17px; flex: none; cursor: pointer; }
  .sw input { opacity: 0; width: 0; height: 0; }
  .sw span { position: absolute; inset: 0; background: var(--line2); border-radius: 9px; transition: 0.15s; }
  .sw span::after { content: ''; position: absolute; width: 13px; height: 13px; border-radius: 50%; background: #fff; top: 2px; left: 2px; transition: 0.15s; }
  .sw input:checked + span { background: var(--acc); }
  .sw input:checked + span::after { left: 15px; }
  .keybox { background: var(--acc-dim); border: 1px solid #f9731640; border-radius: 8px; padding: 10px; display: flex; flex-direction: column; gap: 6px; margin: 6px 0 10px; }
  .key { background: var(--base); padding: 4px 8px; border-radius: 5px; font-size: 11.5px; word-break: break-all; }
  .end { justify-content: flex-end; }
  .sub { margin-top: 12px; padding-top: 10px; border-top: 1px solid var(--line); display: flex; flex-direction: column; gap: 6px; }
  .rcv { padding: 3px 0; }
  .lnk { color: var(--acc); font-size: inherit; padding: 0; }
</style>
