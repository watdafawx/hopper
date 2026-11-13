<script>
  // Settings › Nodes (design/multi-panel.md, A1; mock option A): add a
  // node with a join command or a 6-digit code, see the nodes and remove
  // them. Owner only. When this panel is itself a node, its main is at
  // the top with how the connection is doing.
  import { onMount } from 'svelte';
  import { Check, X, Trash2, Plus, Copy, ServerCog, TriangleAlert, Unplug, RefreshCw } from '@lucide/svelte';
  import { api, post, del } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { copyText } from './lib/browse.js';
  import { CEILINGS, NODE_SIDE, ceilingLabel, nodeLine, clock, codeGroups, defaultAddress, stateOf, updateLine, updateLabel, behind, behindLine } from './lib/nodes.js';

  let { hello } = $props();
  let data = $state(null);
  let self = $state(null);
  let now = $state(Date.now() / 1000);
  // Per waiting request: the ceiling picked before Allow.
  let askCeiling = $state({});

  // The join command dialog.
  let open = $state(false);
  let addr = $state('');
  let ceiling = $state('manage');
  let made = $state(null);
  let os = $state('linux');
  let busy = $state(false);

  async function load() {
    try {
      [data, self] = await Promise.all([api('/nodes'), api('/this-node')]);
      now = Date.now() / 1000;
    } catch (e) {
      if (!data) say(e.message, true);
    }
  }

  onMount(() => {
    load();
    // Codes and states change on their own: a node asks, connects, drops.
    const t = setInterval(load, 2000);
    return () => clearInterval(t);
  });

  function openAdd() {
    addr = defaultAddress(location, hello);
    ceiling = 'manage';
    made = null;
    // Most nodes are rented Linux boxes, whatever this browser runs on.
    os = 'linux';
    open = true;
  }

  async function make() {
    busy = true;
    try {
      made = await post('/nodes/keys', { url: addr.trim(), ceiling });
      load();
    } catch (e) {
      say(e.message, true);
    }
    busy = false;
  }

  async function copy() {
    if (await copyText(made[os])) say('Copied. Paste it on the other machine.');
    else say('Select the command and copy it.', true);
  }

  async function answer(r, allow) {
    try {
      await post(`/nodes/pending/${r.id}/answer`, { allow, ceiling: askCeiling[r.id] ?? 'manage' });
      say(allow ? `${r.name} is now a node of this panel.` : `Refused ${r.name}.`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function remove(n) {
    const ok = await confirmIt(`Remove ${n.name}?`, {
      body: 'This panel forgets it and closes its connection. Its servers keep running there. To add it again, join it with a new command.',
      ok: 'Remove',
      danger: true,
    });
    if (!ok) return;
    try {
      await del(`/nodes/${n.id}`);
      say(`Removed ${n.name}.`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  // Updating runs on the node, through its own update; this panel only
  // asks and follows it (design/multi-panel.md, A3).
  async function update(n) {
    const ok = await confirmIt(`Update ${n.name} to hopperd ${data.version}?`, {
      body: 'It downloads the new Hopper Server from GitHub and restarts into it, as its own update does. Its servers keep running.',
      ok: 'Update',
    });
    if (!ok) return;
    try {
      await post(`/nodes/${n.id}/update`, {});
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function updateAll() {
    const { updatable } = behind(data);
    const ok = await confirmIt(`Update ${updatable === 1 ? '1 node' : `${updatable} nodes`} to hopperd ${data.version}?`, {
      body: 'One at a time, each through its own update. It stops at the first one that fails. Their servers keep running.',
      ok: 'Update all',
    });
    if (!ok) return;
    try {
      await post('/nodes/update-all', {});
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function removeKey(k) {
    try {
      await del(`/nodes/keys/${k.id}`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function leave() {
    const main = self.main_name || self.main_url;
    const ok = await confirmIt(`Leave ${main}?`, {
      body: 'This panel forgets its main and stops connecting to it. Your servers here keep running. To join again you need a new command from the main.',
      ok: 'Leave',
      danger: true,
    });
    if (!ok) return;
    try {
      const r = await del('/this-node');
      say(r.message);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  const mainName = $derived(self?.main_name || self?.main_url || '');
  // A node normally has no nodes of its own: its page shows only its main
  // until someone adds one anyway.
  const showMain = $derived(!self?.joined || !!(data?.nodes?.length || data?.keys?.length || data?.pending?.length));
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && open && !busy && !e.defaultPrevented && (e.preventDefault(), (open = false))} />

<div class="grid">
  {#if self?.joined}
    {@const st = NODE_SIDE[self.status?.state] ?? { label: 'Starting', cls: '' }}
    <div class="card">
      <h3><Unplug size={14} />This panel is a node</h3>
      <div class="row mainrow">
        <span class="dot {st.cls}"></span>
        <div class="grow">
          <div>{mainName}</div>
          <div class="tiny muted ell">{self.main_name ? `${self.main_url} · ` : ''}it may: {ceilingLabel(self.ceiling)}</div>
        </div>
        <span class="chip {st.cls}">{st.label}</span>
        <button class="btn sm" onclick={leave}>Leave</button>
      </div>
      {#if self.status?.message}<p class="small subtle msg" class:bad={self.status.state === 'stopped' || self.status.step === 'certificate'}>{self.status.message}</p>{/if}
      <div class="tiny muted fp">Main's certificate <span class="mono">{self.fingerprint}</span></div>
    </div>
  {/if}

  {#if showMain}
  <div class="card">
    <div class="row head"><h3><ServerCog size={14} />Add a node</h3><span class="sp"></span><button class="btn pri" onclick={openAdd}><Plus size={14} />Make a join command</button></div>
    <p class="small subtle">A node is another machine running Hopper Server. It connects to this panel, so it needs no open port. Or, on that machine, run <span class="mono txt">hopperd node join {defaultAddress(location, hello)}</span> and compare the 6-digit code here.</p>
    {#each data?.keys ?? [] as k (k.id)}
      <div class="row keyrow tiny muted">
        <span>A join command made at {clock(k.created, now)} (it may: {ceilingLabel(k.ceiling)}) works until {clock(k.expires, now)}.</span>
        <span class="sp"></span><button class="btn sm ghost" onclick={() => removeKey(k)}>Cancel it</button>
      </div>
    {/each}
  </div>

  {#if data?.pending?.length}
    <div class="card">
      <h3>Waiting for you</h3>
      {#each data.pending as r (r.id)}
        <div class="req">
          <div class="small subtle"><b class="txt">{r.name}</b> wants to join from <span class="mono">{r.ip}</span>. Check it shows the same code.</div>
          <div class="code" aria-label="Code {r.code}">{#each codeGroups(r.code) as g, gi (gi)}{#if gi}<i></i>{/if}{#each g.split('') as c, i (i)}<span>{c}</span>{/each}{/each}</div>
          <div class="row center ask">
            <span class="small muted">It may</span>
            <div class="seg" role="group" aria-label="What this panel may do there">
              {#each CEILINGS as c (c.id)}<button aria-pressed={(askCeiling[r.id] ?? 'manage') === c.id} onclick={() => (askCeiling[r.id] = c.id)}>{c.label}</button>{/each}
            </div>
          </div>
          <div class="row center">
            <button class="btn" onclick={() => answer(r, false)}><X size={14} />Refuse</button>
            <button class="btn pri" onclick={() => answer(r, true)}><Check size={14} />Allow {r.name}</button>
          </div>
        </div>
      {/each}
    </div>
  {/if}

  <div class="card list">
    {#if behindLine(data)}
      {@const b = behind(data)}
      {@const all = data.update_all}
      <div class="row node behind">
        <div class="grow">
          <div class="small subtle">{behindLine(data)}</div>
          {#if all?.message || all?.state === 'running'}<div class="tiny ell" class:bad={all.state === 'stopped'} class:muted={all.state !== 'stopped'}>{all.state === 'running' ? `Updating ${all.at ?? ''} (${all.done + 1} of ${all.total})` : all.message}</div>{/if}
        </div>
        {#if b.updatable > 1}<button class="btn sm" disabled={all?.state === 'running'} onclick={updateAll}><RefreshCw size={13} />Update all ({b.updatable})</button>{/if}
      </div>
    {/if}
    {#each data?.nodes ?? [] as n (n.id)}
      {@const st = stateOf(n)}
      {@const up = updateLine(n)}
      <div class="row node row-hover">
        <span class="dot {st.cls}"></span>
        <div class="grow">
          <div class="ell">{n.name}</div>
          {#if up}<div class="tiny ell" class:bad={up.bad} class:muted={!up.bad} title={up.text}>{up.text}</div>
          {:else}<div class="tiny muted ell" title={nodeLine(n, now)}>{nodeLine(n, now)}</div>{/if}
        </div>
        <span class="chip state {st.cls}">{st.label}</span>
        <span class="chip" title="What this panel may do there">{ceilingLabel(n.ceiling)}</span>
        {#if n.can_update}<button class="btn sm" onclick={() => update(n)}><RefreshCw size={13} />{updateLabel(n, data.version)}</button>{/if}
        <button class="btn sm ghost icon" title="Remove {n.name}" aria-label="Remove {n.name}" onclick={() => remove(n)}><Trash2 size={13} /></button>
      </div>
    {:else}
      {#if data}
        <div class="empty-state"><div class="es-ic"><ServerCog size={18} /></div><b>No nodes yet</b><p>Make a join command and paste it on the other machine. It shows up here within seconds.</p></div>
      {:else}
        <div class="skel"><i></i></div><div class="skel"><i></i></div>
      {/if}
    {/each}
  </div>
  {/if}
</div>

{#if open}
  <div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && (open = false)}>
    <div class="box popover" role="dialog" aria-modal="true" aria-label="Add a node">
      <h3><ServerCog size={15} />Add a node<span class="sp"></span><button class="btn sm icon ghost" title="Close" aria-label="Close" onclick={() => (open = false)}><X size={14} /></button></h3>
      {#if !made}
        <div class="form">
          <label class="small subtle" for="nd-addr">This panel's address</label>
          <div class="input"><input id="nd-addr" class="mono" bind:value={addr} spellcheck="false" /></div>
          <span class="small subtle">It may</span>
          <div class="seg" role="group" aria-label="What this panel may do there">
            {#each CEILINGS as c (c.id)}<button aria-pressed={ceiling === c.id} onclick={() => (ceiling = c.id)}>{c.label}</button>{/each}
          </div>
        </div>
        <p class="tiny muted">The address is how the other machine reaches this panel. {CEILINGS.find((c) => c.id === ceiling)?.blurb}</p>
        {#if ceiling === 'manage'}<p class="tiny warn"><TriangleAlert size={12} />Anyone who can sign in to this panel as an admin could run code on that machine.</p>{/if}
        <div class="row end"><span class="sp"></span><button class="btn" onclick={() => (open = false)}>Cancel</button><button class="btn pri" disabled={busy || !addr.trim()} onclick={make}>Make the command</button></div>
      {:else}
        <div class="utabs" role="tablist">
          <button role="tab" aria-selected={os === 'linux'} class:tab-sel={os === 'linux'} onclick={() => (os = 'linux')}>Linux</button>
          <button role="tab" aria-selected={os === 'windows'} class:tab-sel={os === 'windows'} onclick={() => (os = 'windows')}>Windows</button>
        </div>
        <p class="tiny muted">{os === 'linux' ? 'In a terminal on the other machine (it asks for your sudo password). It installs Hopper Server as a service and joins.' : 'In PowerShell, run as administrator, on the other machine. It installs Hopper Server as a service and joins.'}</p>
        <pre class="cmd mono">{made[os]}</pre>
        <p class="tiny muted">Works once, for 30 minutes (until {clock(made.expires)}). It is shown only now. It may: {ceilingLabel(ceiling)}.</p>
        <div class="row end"><span class="sp"></span><button class="btn" onclick={() => (open = false)}>Done</button><button class="btn pri" onclick={copy}><Copy size={13} />Copy</button></div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .grid { display: flex; flex-direction: column; gap: 14px; }
  .txt { color: var(--text); }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .head { margin-bottom: 6px; }
  .head h3 { margin: 0; }
  .keyrow { margin-top: 8px; padding-top: 8px; border-top: 1px solid var(--line); gap: 8px; }
  .req { padding-bottom: 12px; border-bottom: 1px solid var(--line); margin-bottom: 12px; }
  .req:last-child { border: 0; margin: 0; padding: 0; }
  .code { display: flex; gap: 8px; justify-content: center; margin: 14px 0; }
  .code span { width: 42px; height: 52px; border-radius: var(--radius); background: var(--base); border: 1px solid var(--line2); display: grid; place-items: center; font: 600 24px var(--mono); }
  .code i { width: 8px; }
  .center { justify-content: center; }
  .ask { margin-bottom: 12px; gap: 10px; }
  /* A few values to pick from: the chosen one in the selected tint (desktop's widgets::Segmented). */
  .seg { display: inline-flex; gap: 2px; padding: 2px; border-radius: var(--radius); background: var(--hover); width: max-content; }
  .seg button { height: 26px; padding: 0 10px; border-radius: var(--radius-sm); color: var(--muted); font-size: 12.5px; }
  .seg button:hover { color: var(--text); }
  /* The tint marks the choice; the text stays --text so it reads in light themes too. */
  .seg button[aria-pressed='true'] { background: var(--sel); color: var(--text); font-weight: 600; }
  .seg button:focus-visible { outline: 2px solid var(--acc); outline-offset: -2px; }
  .list { padding: 4px 6px; }
  .node { gap: 10px; padding: 10px 8px; border-radius: var(--radius); }
  .node + .node { border-top: 1px solid var(--line); border-radius: 0; }
  .behind { padding: 8px; }
  .bad { color: var(--red); }
  .mainrow { gap: 10px; }
  .msg { margin-top: 10px; }
  .msg.bad { color: var(--red); }
  .fp { margin-top: 8px; word-break: break-all; }
  .box { width: min(560px, 100%); display: flex; flex-direction: column; gap: 10px; padding: 16px; }
  .box h3 { margin: 0; display: flex; align-items: center; gap: 8px; font-size: 15px; }
  .form { display: grid; grid-template-columns: 140px minmax(0, 1fr); gap: 10px 12px; align-items: center; }
  .warn { color: var(--subtle); display: flex; align-items: center; gap: 6px; }
  .warn :global(svg) { color: var(--gold); flex: none; }
  .cmd { margin: 0; padding: 10px; border-radius: var(--radius); background: var(--term); border: 1px solid var(--line); font-size: 11.5px; white-space: pre-wrap; word-break: break-all; user-select: all; }
  .end { gap: 8px; }
  .empty-state :global(svg) { display: block; }
  @media (max-width: 600px) {
    .form { grid-template-columns: 1fr; gap: 6px; }
    .node { flex-wrap: wrap; }
    .node .grow { flex-basis: calc(100% - 18px); }
    .node .state { margin-left: 18px; }
    .node button { margin-left: auto; }
    .head { flex-wrap: wrap; gap: 8px; }
  }
</style>
