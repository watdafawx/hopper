<script>
  import { onMount } from 'svelte';
  import { Webhook, Plus, Trash2, Send, Pencil, AlertTriangle } from '@lucide/svelte';
  import { api, post, del } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';

  let hooks = $state([]);
  let events = $state([]);
  let servers = $state([]);
  let form = $state(null);

  async function load() {
    try {
      const r = await api('/webhooks');
      hooks = r.hooks;
      events = r.events;
    } catch (e) {
      say(e.message, true);
    }
  }

  function edit(h) {
    form = h
      ? { id: h.id, name: h.name, url: h.url, events: [...h.events], servers: [...h.servers], enabled: h.enabled }
      : { name: '', url: '', events: ['start', 'stop', 'crash', 'backup', 'problem', 'tps', 'disk', 'ram', 'copy_failed'], servers: [], enabled: true };
  }

  const flip = (list, v) => (list.includes(v) ? list.filter((x) => x !== v) : [...list, v]);

  async function save(e) {
    e.preventDefault();
    try {
      const body = { name: form.name, url: form.url, events: form.events, servers: form.servers, enabled: form.enabled };
      if (form.id) await api(`/webhooks/${form.id}`, { method: 'PUT', body });
      else await post('/webhooks', body);
      form = null;
      load();
    } catch (err) {
      say(err.message, true);
    }
  }

  async function test(h) {
    try {
      await post(`/webhooks/${h.id}/test`);
      say(`Sent a test to ${h.name}`);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function remove(h) {
    if (!(await confirmIt(`Remove ${h.name}?`, { ok: 'Remove', danger: true }))) return;
    try {
      await del(`/webhooks/${h.id}`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function toggle(h) {
    try {
      await api(`/webhooks/${h.id}`, { method: 'PUT', body: { ...h, enabled: !h.enabled } });
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  onMount(async () => {
    load();
    try {
      servers = await api('/servers');
    } catch {}
  });
</script>

<div class="card">
  <h3><Webhook size={14} />Webhooks <span class="sp"></span>
    {#if !form}<button class="btn sm" onclick={() => edit(null)}><Plus size={13} />Add</button>{/if}</h3>
  {#if form}
    <form class="col" onsubmit={save}>
      <div class="input"><input bind:value={form.url} placeholder="https://discord.com/api/webhooks/…" required /></div>
      <div class="input"><input bind:value={form.name} placeholder="Name (optional)" /></div>
      <div class="tiny muted">Discord URLs get Discord messages; any other URL gets JSON: event, server, text, at.</div>
      <div class="chips">
        {#each events as ev (ev.id)}
          <button type="button" class="chip" class:acc={form.events.includes(ev.id)} onclick={() => (form.events = flip(form.events, ev.id))}>{ev.label}</button>
        {/each}
      </div>
      {#if servers.length > 1}
        <div class="tiny muted">Servers {form.servers.length ? '' : '(all)'}</div>
        <div class="chips">
          {#each servers as s (s.id)}
            <button type="button" class="chip" class:acc={form.servers.includes(s.id)} onclick={() => (form.servers = flip(form.servers, s.id))}>{s.name}</button>
          {/each}
        </div>
      {/if}
      <div class="row"><span class="sp"></span>
        <button type="button" class="btn sm" onclick={() => (form = null)}>Cancel</button>
        <button class="btn sm pri" disabled={!form.url || !form.events.length}>Save</button></div>
    </form>
  {:else}
    {#each hooks as h (h.id)}
      <div class="hook" class:off={!h.enabled}>
        <div class="grow">
          <div>{h.name} <span class="chip">{h.kind === 'discord' ? 'Discord' : 'JSON'}</span></div>
          <div class="tiny muted ell">{h.events.join(', ')}{h.servers.length ? ` · ${h.servers.length} server${h.servers.length === 1 ? '' : 's'}` : ''}</div>
          {#if h.last_error}<div class="tiny bad"><AlertTriangle size={11} /> {h.last_error}</div>{/if}
        </div>
        <label class="sw" title={h.enabled ? 'Turn off' : 'Turn on'}><input type="checkbox" checked={h.enabled} onchange={() => toggle(h)} /><span></span></label>
        <button class="btn sm icon ghost" title="Send a test" onclick={() => test(h)}><Send size={13} /></button>
        <button class="btn sm icon ghost" title="Edit" onclick={() => edit(h)}><Pencil size={13} /></button>
        <button class="btn sm icon ghost" title="Remove" onclick={() => remove(h)}><Trash2 size={13} /></button>
      </div>
    {:else}
      <p class="small subtle">Post to Discord or any URL when servers start, stop or crash, players join or leave, backups finish, and alerts fire.</p>
    {/each}
  {/if}
</div>

<style>
  .col { display: flex; flex-direction: column; gap: 8px; }
  .chips { display: flex; flex-wrap: wrap; gap: 5px; }
  .chips .chip { cursor: pointer; }
  .hook { display: flex; align-items: center; gap: 8px; padding: 8px 0; border-bottom: 1px solid var(--line); }
  .hook:last-child { border: 0; }
  .hook.off { opacity: 0.55; }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .bad { color: var(--red); display: flex; align-items: center; gap: 4px; }
  .sw { position: relative; width: 30px; height: 17px; flex: none; cursor: pointer; }
  .sw input { opacity: 0; width: 0; height: 0; }
  .sw span { position: absolute; inset: 0; background: var(--line2); border-radius: 9px; transition: 0.15s; }
  .sw span::after { content: ''; position: absolute; width: 13px; height: 13px; border-radius: 50%; background: #fff; top: 2px; left: 2px; transition: 0.15s; }
  .sw input:checked + span { background: var(--acc); }
  .sw input:checked + span::after { left: 15px; }
</style>
