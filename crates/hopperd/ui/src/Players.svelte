<script>
  import { Crown, UserX, Ban, ShieldCheck, UserPlus, Undo2, LogOut } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { ask } from './lib/dialog.svelte.js';

  let { server } = $props();
  let data = $state(null);
  let name = $state('');

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const ops = $derived(new Set((data?.ops || []).map((p) => p.name.toLowerCase())));

  async function load() {
    try {
      data = await api(`${base}/players`);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function act(action, who, reason = '') {
    try {
      await post(`${base}/players`, { action, name: who, reason });
      say(`${who}: done`);
      setTimeout(load, 700);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function kickOrBan(action, who) {
    const reason = await ask(`${action === 'kick' ? 'Kick' : 'Ban'} ${who}`, '', {
      body: 'Reason (optional), shown to the player.',
      ok: action === 'kick' ? 'Kick' : 'Ban',
    });
    if (reason !== null) act(action, who, reason);
  }

  async function add(action) {
    const n = name.trim();
    if (!n) return;
    await act(action, n);
    name = '';
  }

  $effect(() => {
    server.id;
    server.players.length;
    load();
  });

  const head = (n) => `https://mc-heads.net/avatar/${encodeURIComponent(n)}/22`;
</script>

<div class="grid">
  <div class="card">
    <h3>Online <span class="muted fw">{server.players.length} / {server.max_players}</span></h3>
    {#each server.players as p (p)}
      <div class="pl">
        <img src={head(p)} alt="" width="22" height="22" loading="lazy" />
        <span class="nm">{p}</span>
        {#if ops.has(p.toLowerCase())}<span class="chip acc">op</span>{/if}
        <span class="sp"></span>
        {#if ops.has(p.toLowerCase())}
          <button class="btn sm ghost" title="Remove op" onclick={() => act('deop', p)}><Crown size={13} />Deop</button>
        {:else}
          <button class="btn sm ghost" title="Make operator" onclick={() => act('op', p)}><Crown size={13} />Op</button>
        {/if}
        <button class="btn sm ghost" onclick={() => kickOrBan('kick', p)}><LogOut size={13} />Kick</button>
        <button class="btn sm ghost bad" onclick={() => kickOrBan('ban', p)}><Ban size={13} />Ban</button>
      </div>
    {:else}
      <p class="small muted">{data?.running ? 'Nobody online.' : 'The server is stopped.'}</p>
    {/each}
  </div>

  <div class="card">
    <h3>Add a player</h3>
    <div class="row">
      <div class="input grow"><input bind:value={name} placeholder="Minecraft name" spellcheck="false" autocomplete="off" /></div>
    </div>
    <div class="row wrap">
      <button class="btn sm" disabled={!name.trim()} onclick={() => add('whitelist_add')}><UserPlus size={13} />Whitelist</button>
      <button class="btn sm" disabled={!name.trim()} onclick={() => add('op')}><Crown size={13} />Op</button>
      <button class="btn sm bad" disabled={!name.trim()} onclick={() => add('ban')}><Ban size={13} />Ban</button>
    </div>
    {#if data && !data.running}<p class="tiny muted">Start the server to change players: it looks names up and saves its lists itself.</p>{/if}
  </div>

  <div class="card">
    <h3><ShieldCheck size={14} />Whitelist <span class="chip" class:running={data?.whitelist_on}>{data?.whitelist_on ? 'on' : 'off'}</span></h3>
    {#each data?.whitelist || [] as p (p.name)}
      <div class="pl"><img src={head(p.name)} alt="" width="22" height="22" loading="lazy" /><span class="nm">{p.name}</span><span class="sp"></span>
        <button class="btn sm icon ghost" title="Remove" onclick={() => act('whitelist_remove', p.name)}><UserX size={13} /></button></div>
    {:else}<p class="small muted">Nobody yet.{data && !data.whitelist_on ? ' Turn it on in Settings (white-list).' : ''}</p>{/each}
  </div>

  <div class="card">
    <h3><Crown size={14} />Operators</h3>
    {#each data?.ops || [] as p (p.name)}
      <div class="pl"><img src={head(p.name)} alt="" width="22" height="22" loading="lazy" /><span class="nm">{p.name}</span>
        <span class="tiny muted">level {p.level ?? 4}</span><span class="sp"></span>
        <button class="btn sm icon ghost" title="Remove op" onclick={() => act('deop', p.name)}><UserX size={13} /></button></div>
    {:else}<p class="small muted">None.</p>{/each}
  </div>

  <div class="card">
    <h3><Ban size={14} />Banned</h3>
    {#each data?.banned || [] as p (p.name)}
      <div class="pl"><img src={head(p.name)} alt="" width="22" height="22" loading="lazy" /><span class="nm">{p.name}</span>
        <span class="tiny muted ell">{p.reason || ''}</span><span class="sp"></span>
        <button class="btn sm ghost" onclick={() => act('pardon', p.name)}><Undo2 size={13} />Unban</button></div>
    {:else}<p class="small muted">Nobody.</p>{/each}
  </div>
</div>

<style>
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr)); gap: 12px; align-content: start; }
  .card { display: flex; flex-direction: column; gap: 8px; }
  .card h3 { margin-bottom: 4px; }
  .fw { font-weight: 400; }
  .pl { display: flex; align-items: center; gap: 8px; padding: 4px 0; border-bottom: 1px solid var(--line); }
  .pl:last-child { border: 0; }
  .pl img { border-radius: 4px; background: var(--raise); }
  .nm { font-weight: 500; }
  .grow { flex: 1; }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 140px; }
</style>
