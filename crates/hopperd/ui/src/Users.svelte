<script>
  import { onMount } from 'svelte';
  import { Users, Plus, Pencil, Trash2, ShieldOff } from '@lucide/svelte';
  import { api, post, del } from './lib/api.js';
  import { live } from './lib/live.svelte.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { session, roleLabel } from './lib/me.svelte.js';

  const ALL_ROLES = [
    { id: 'manager', label: 'Manager', about: 'Everything the owner can do, on every server, except managing the owner and other managers.' },
    { id: 'admin', label: 'Admin', about: 'Everything on their servers: files, mods, settings, backups.' },
    { id: 'operator', label: 'Operator', about: 'Start and stop, console, players, make backups. Can look at files.' },
    { id: 'viewer', label: 'Viewer', about: 'Looks only: console, files, stats.' },
  ];
  // Only the owner makes managers, or changes and removes them.
  const ROLES = $derived(session.trueOwner ? ALL_ROLES : ALL_ROLES.filter((r) => r.id !== 'manager'));
  const canManage = (u) => session.trueOwner || u.role !== 'manager';

  let users = $state([]);
  let form = $state(null);

  async function load() {
    try {
      users = await api('/users');
    } catch (e) {
      say(e.message, true);
    }
  }

  function edit(u) {
    form = u ? { id: u.id, name: u.name, password: '', role: u.role, servers: [...u.servers] } : { name: '', password: '', role: 'operator', servers: [] };
  }

  const flip = (list, v) => (list.includes(v) ? list.filter((x) => x !== v) : [...list, v]);

  async function save(e) {
    e.preventDefault();
    try {
      const body = { name: form.name, password: form.password, role: form.role, servers: form.servers };
      if (form.id) await api(`/users/${form.id}`, { method: 'PUT', body });
      else await post('/users', body);
      say(form.id ? `Saved ${form.name}` : `Added ${form.name}. They sign in with that name and password.`);
      form = null;
      load();
    } catch (err) {
      say(err.message, true);
    }
  }

  async function remove(u) {
    if (!(await confirmIt(`Remove ${u.name}?`, { body: "They're signed out right away.", ok: 'Remove', danger: true }))) return;
    try {
      await del(`/users/${u.id}`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function reset2fa(u) {
    if (!(await confirmIt(`Turn off two-factor sign-in for ${u.name}?`, { body: 'For a lost phone. They can set it up again.', ok: 'Turn off' }))) return;
    try {
      await post(`/users/${u.id}/reset-2fa`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  const when = (t) => (t ? new Date(t * 1000).toLocaleString() : 'never');
  const names = (ids) => ids.map((id) => live.servers.find((s) => s.id === id)?.name || id).join(', ');

  onMount(load);
</script>

<div class="card">
  <h3><Users size={14} />People <span class="sp"></span>
    {#if !form}<button class="btn sm" onclick={() => edit(null)}><Plus size={13} />Add</button>{/if}</h3>
  {#if form}
    <form class="col" onsubmit={save}>
      <div class="row">
        <div class="input grow"><input bind:value={form.name} placeholder="Name they sign in with" autocomplete="off" required /></div>
        <div class="input grow"><input type="password" bind:value={form.password} placeholder={form.id ? 'New password (optional)' : 'Password'} autocomplete="new-password" /></div>
      </div>
      <div class="roles">
        {#each ROLES as r (r.id)}
          <button type="button" class="role" class:on={form.role === r.id} onclick={() => (form.role = r.id)}>
            <b class="small">{r.label}</b><span class="tiny muted">{r.about}</span>
          </button>
        {/each}
      </div>
      {#if live.servers.length > 1 && form.role !== 'manager'}
        <div class="row wrap"><span class="tiny muted">Servers {form.servers.length ? '' : '(all)'}</span>
          {#each live.servers as s (s.id)}
            <button type="button" class="chip" class:acc={form.servers.includes(s.id)} onclick={() => (form.servers = flip(form.servers, s.id))}>{s.name}</button>
          {/each}
        </div>
      {/if}
      <div class="row"><span class="sp"></span>
        <button type="button" class="btn sm" onclick={() => (form = null)}>Cancel</button>
        <button class="btn sm pri" disabled={!form.name.trim() || (!form.id && !form.password)}>{form.id ? 'Save' : 'Add'}</button></div>
    </form>
  {:else}
    {#each users as u (u.id)}
      <div class="u">
        <div class="grow">
          <div>{u.name} <span class="chip">{roleLabel(u.role)}</span>{#if u.two_factor}<span class="chip running">2FA</span>{/if}</div>
          <div class="tiny muted ell">{u.servers.length && u.role !== 'manager' ? names(u.servers) : 'All servers'} · last seen {when(u.last_seen)}</div>
        </div>
        {#if canManage(u)}
        {#if u.two_factor}<button class="btn sm icon ghost" title="Turn off their two-factor sign-in" onclick={() => reset2fa(u)}><ShieldOff size={13} /></button>{/if}
        <button class="btn sm icon ghost" title="Edit" onclick={() => edit(u)}><Pencil size={13} /></button>
        <button class="btn sm icon ghost" title="Remove" onclick={() => remove(u)}><Trash2 size={13} /></button>
        {/if}
      </div>
    {:else}
      <p class="small subtle">Let others in with their own name and password, limited to what their role allows and to the servers you pick.</p>
    {/each}
  {/if}
</div>

<style>
  .col { display: flex; flex-direction: column; gap: 8px; }
  .grow { flex: 1; min-width: 0; }
  .wrap { flex-wrap: wrap; }
  .roles { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 6px; }
  .role { display: flex; flex-direction: column; gap: 2px; text-align: left; padding: 8px; border: 1px solid var(--line2); border-radius: 8px; }
  .role.on { border-color: var(--acc); background: var(--acc-dim); }
  .u { display: flex; align-items: center; gap: 8px; padding: 8px 0; border-bottom: 1px solid var(--line); }
  .u:last-child { border: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  @media (max-width: 600px) { .roles { grid-template-columns: 1fr; } }
</style>
