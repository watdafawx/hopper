<script>
  // Settings › Other machines (mock option A): this panel's nodes and the
  // panels signed in to on this page, in one list, trouble first. Underneath
  // they stay two kinds (design/multi-panel.md: a node dials in to this
  // panel, a signed-in panel is reached by this browser); the chip says
  // which. Add a machine offers a node first and signing in second
  // (AddMachine.svelte).
  //
  // The node parts (join requests, join commands, update, remove) are the
  // real owner's only: a node runs code for this panel. Everyone signed in
  // with a password sees the list and keeps their own signed-in panels.
  // When this panel is itself a node, its main is at the top.
  import { onMount, tick } from 'svelte';
  import { Check, X, Plus, Unplug, RefreshCw, Ellipsis, ExternalLink, RotateCw, LogOut, Power, Trash2, Pause, Play, LogIn, MonitorSmartphone } from '@lucide/svelte';
  import { api, post, del } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { session, roleLabel } from './lib/me.svelte.js';
  import { live } from './lib/live.svelte.js';
  import { panels, retry, signOut, setOff } from './lib/panels.svelte.js';
  import { tokenFor } from './lib/panelTokens.js';
  import { SWITCH_TIP } from './lib/panelOff.js';
  import { CEILINGS, NODE_SIDE, ceilingLabel, clock, codeGroups, behind, behindLine } from './lib/nodes.js';
  import { KIND, machines, needYou, serverCount, panelLine, nodeRowLine, kindTip } from './lib/machines.js';
  import { updateNode, maintainNode, removeNode, dropPanel } from './lib/machineActions.js';
  import AddMachine from './AddMachine.svelte';
  import PanelSignIn from './PanelSignIn.svelte';

  let { hello } = $props();
  let data = $state(null);
  let self = $state(null);
  let now = $state(Date.now() / 1000);
  // Per waiting request: the ceiling picked before Allow.
  let askCeiling = $state({});
  let adding = $state(false);
  // The signed-in panel whose sign-in form is open under its row.
  let signing = $state('');

  async function load() {
    // The node routes are the real owner's; everyone else sees the nodes
    // the live feed shows them.
    if (!session.trueOwner) return;
    try {
      [data, self] = await Promise.all([api('/nodes', { home: true }), api('/this-node', { home: true })]);
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

  // The live feed's node heads carry the server count and the machine's
  // load; /nodes adds when it joined, its address and the join details.
  const nodes = $derived.by(() => {
    const heads = new Map((live.nodes || []).map((n) => [n.id, n]));
    const rows = (data?.nodes ?? []).map((d) => ({ ...d, ...(heads.get(d.id) ?? {}) }));
    for (const n of heads.values()) if (!rows.some((r) => r.id === n.id)) rows.push(n);
    return rows;
  });
  const list = $derived(machines(nodes, panels.list));
  const need = $derived(needYou(list));
  const ready = $derived(!session.trueOwner || !!data);
  const version = $derived(data?.version || hello?.version || '');

  async function answer(r, allow) {
    try {
      await post(`/nodes/pending/${r.id}/answer`, { allow, ceiling: askCeiling[r.id] ?? 'manage' }, { home: true });
      say(allow ? `${r.name} is now a node of this panel.` : `Refused ${r.name}.`);
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
      await post('/nodes/update-all', {}, { home: true });
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function removeKey(k) {
    try {
      await del(`/nodes/keys/${k.id}`, { home: true });
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
      const r = await del('/this-node', { home: true });
      say(r.message);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  const mainName = $derived(self?.main_name || self?.main_url || '');

  /** The one thing to do about a signed-in panel, as a button on its row. */
  function act(m, line) {
    if (!line.act || m.panel.off) return null;
    if (line.act === 'retry') return { label: 'Try again', icon: RotateCw, run: () => retry(m.panel) };
    if (line.act === 'signin') return { label: signing === m.key ? 'Cancel' : 'Sign in', icon: signing === m.key ? null : LogIn, run: () => (signing = signing === m.key ? '' : m.key) };
    return { label: 'Open its page', icon: ExternalLink, run: () => window.open(m.panel.url, '_blank', 'noopener') };
  }

  // ---- a row's menu ----
  let menu = $state(null); // { m, x, y }
  let menuEl = $state();

  async function openMenu(e, m) {
    const r = e.currentTarget.getBoundingClientRect();
    menu = { m, x: r.right, y: r.bottom + 4 };
    await tick();
    if (!menuEl) return;
    const b = menuEl.getBoundingClientRect();
    menu.x = Math.max(8, Math.min(r.right - b.width, innerWidth - b.width - 8));
    menu.y = r.bottom + 4 + b.height > innerHeight - 8 ? r.top - 4 - b.height : r.bottom + 4;
    menuEl.querySelector('button')?.focus();
  }

  function pick(f) {
    const m = menu?.m;
    menu = null;
    if (m) f(m);
  }

  function menuKeys(e) {
    if (e.key === 'Escape' || e.key === 'Tab') {
      e.preventDefault();
      menu = null;
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const items = [...menuEl.querySelectorAll('button')];
      const i = items.indexOf(document.activeElement);
      items[e.key === 'ArrowDown' ? (i + 1) % items.length : (i - 1 + items.length) % items.length]?.focus();
    }
  }

  const away = (e) => menu && menuEl && !menuEl.contains(e.target) && !e.target.closest?.('.more') && (menu = null);
  /** Whether a row has anything in its menu for this person. */
  const hasMenu = (m) => m.kind === 'panel' || session.owner;
</script>

<svelte:window onpointerdown={away} onresize={() => (menu = null)} onblur={() => (menu = null)} />

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

  {#if data?.pending?.length}
    <div class="card">
      <h3>Waiting for you</h3>
      {#each data.pending as r (r.id)}
        <div class="req">
          <div class="small subtle"><b class="txt">{r.name}</b> wants to join as a node from <span class="mono">{r.ip}</span>. Check it shows the same code.</div>
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
    <div class="row top">
      <span class="small subtle">{ready ? `${list.length === 1 ? '1 machine' : `${list.length} machines`}${need ? ` · ${need} ${need === 1 ? 'needs' : 'need'} you` : ''}` : ''}</span>
      <span class="sp"></span>
      <button class="btn pri" onclick={() => (adding = true)}><Plus size={14} />Add a machine</button>
    </div>
    {#each data?.keys ?? [] as k (k.id)}
      <div class="row keyrow tiny muted">
        <span>A join command made at {clock(k.created, now)} (it may: {ceilingLabel(k.ceiling)}) works until {clock(k.expires, now)}.</span>
        <span class="sp"></span><button class="btn sm ghost" onclick={() => removeKey(k)}>Cancel it</button>
      </div>
    {/each}
    {#if behindLine(data)}
      {@const b = behind(data)}
      {@const all = data.update_all}
      <div class="row behind">
        <div class="grow">
          <div class="small subtle">{behindLine(data)}</div>
          {#if all?.message || all?.state === 'running'}<div class="tiny ell" class:bad={all.state === 'stopped'} class:muted={all.state !== 'stopped'}>{all.state === 'running' ? `Updating ${all.at ?? ''} (${all.done + 1} of ${all.total})` : all.message}</div>{/if}
        </div>
        {#if b.updatable > 1}<button class="btn sm" disabled={all?.state === 'running'} onclick={updateAll}><RefreshCw size={13} />Update all ({b.updatable})</button>{/if}
      </div>
    {/if}
    {#each list as m (m.key)}
      {@const line = m.kind === 'node' ? nodeRowLine(m.node, now) : panelLine(m.panel, m.panel.me ? roleLabel(m.panel.role) : '')}
      {@const a = m.kind === 'panel' ? act(m, line) : null}
      <div class="mrow row-hover" class:off={m.panel?.off}>
        <span class="dot {m.light}"></span>
        <div class="main">
          <div class="nm" title={m.name}>{m.name}</div>
          <div class="ln tiny" class:bad={line.bad} class:muted={!line.bad}>{line.text}</div>
        </div>
        <div class="meta">
          <span class="chip" title={kindTip(m.kind, { ceiling: m.node?.ceiling, role: m.panel?.me ? roleLabel(m.panel.role) : '' })}>{KIND[m.kind]}</span>
          <span class="cnt tiny muted">{serverCount(m.count) || '–'}</span>
          <span class="act">
            {#if m.kind === 'node' && m.node.can_update && session.trueOwner}<button class="btn sm" onclick={() => updateNode(m.node, version)}><RefreshCw size={13} />Update</button>{/if}
            {#if a}{@const Icon = a.icon}<button class="btn sm" onclick={a.run}>{#if Icon}<Icon size={13} />{/if}{a.label}</button>{/if}
          </span>
        </div>
        {#if hasMenu(m)}<button class="btn sm ghost icon more" title="More" aria-label="More for {m.name}" aria-haspopup="menu" aria-expanded={menu?.m.key === m.key} onclick={(e) => (menu?.m.key === m.key ? (menu = null) : openMenu(e, m))}><Ellipsis size={14} /></button>
        {:else}<span class="more"></span>{/if}
      </div>
      {#if signing === m.key && m.panel?.state === 'signin'}<div class="signin"><PanelSignIn panel={m.panel} /></div>{/if}
    {:else}
      {#if ready}
        <div class="empty-state"><div class="es-ic"><MonitorSmartphone size={18} /></div><b>No other machines yet</b><p>Add a machine and its servers show in the server list, next to this panel's.</p></div>
      {:else}
        <div class="skel"><i></i></div><div class="skel"><i></i></div>
      {/if}
    {/each}
  </div>
</div>

{#if adding}<AddMachine onClose={() => (adding = false)} onMade={load} />{/if}

{#if menu}
  {@const m = menu.m}
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div class="popover menu rmenu" role="menu" tabindex="-1" bind:this={menuEl} style:left="{menu.x}px" style:top="{menu.y}px" onkeydown={menuKeys}>
    {#if m.kind === 'node'}
      {@const n = m.node}
      {#if n.maintenance}<button class="menu-item" role="menuitem" onclick={() => pick((v) => maintainNode(v.node, false))}><Play size={14} />End maintenance</button>
      {:else}<button class="menu-item" role="menuitem" onclick={() => pick((v) => maintainNode(v.node, true))}><Pause size={14} />Pause node (maintenance)</button>{/if}
      {#if session.trueOwner}
        {#if n.can_update}<button class="menu-item" role="menuitem" onclick={() => pick((v) => updateNode(v.node, version))}><RefreshCw size={14} />Update to hopperd {version}</button>{/if}
        <div class="menu-sep"></div>
        <button class="menu-item bad" role="menuitem" onclick={() => pick((v) => removeNode(v.node).then((ok) => ok && load()))}><Trash2 size={14} />Remove {n.name}…</button>
      {/if}
    {:else}
      {@const p = m.panel}
      <button class="menu-item" role="menuitem" onclick={() => pick((v) => window.open(v.panel.url, '_blank', 'noopener'))}><ExternalLink size={14} />Open its page</button>
      {#if !p.off && p.state !== 'ok' && p.state !== 'signin'}<button class="menu-item" role="menuitem" onclick={() => pick((v) => retry(v.panel))}><RotateCw size={14} />Try again</button>{/if}
      {#if !p.off && tokenFor(p.url) && p.state !== 'signin'}<button class="menu-item" role="menuitem" onclick={() => pick((v) => signOut(v.panel))}><LogOut size={14} />Sign out of it</button>{/if}
      <button class="menu-item" role="menuitem" title={SWITCH_TIP} onclick={() => pick((v) => setOff(v.panel, !v.panel.off))}><Power size={14} />{p.off ? 'Turn on' : 'Turn off'}</button>
      <div class="menu-sep"></div>
      <button class="menu-item bad" role="menuitem" onclick={() => pick((v) => dropPanel(v.panel))}><Trash2 size={14} />Remove from this page…</button>
    {/if}
  </div>
{/if}

<style>
  .grid { display: flex; flex-direction: column; gap: 14px; }
  .txt { color: var(--text); }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .top { gap: 10px; padding: 6px 8px 8px; }
  .keyrow { padding: 8px; border-top: 1px solid var(--line); gap: 8px; }
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
  .behind { padding: 8px; gap: 10px; border-top: 1px solid var(--line); }
  /* One machine: light, name over its state, then how it's connected, its
     servers and what to do. The name takes what's left and cuts with an
     ellipsis; the state line wraps so no reason is ever cut off. */
  .mrow { display: grid; grid-template-columns: 8px minmax(0, 1fr) auto 26px; grid-template-areas: 'dot main meta more'; column-gap: 12px; align-items: center; padding: 10px 8px; border-top: 1px solid var(--line); }
  .mrow > .dot { grid-area: dot; }
  .main { grid-area: main; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
  .nm { font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .mrow.off .nm { color: var(--muted); }
  .ln { overflow-wrap: anywhere; }
  .bad { color: var(--red); }
  /* Small red text on the card measured 4.4:1 in both themes; a little of the
     text colour lifts it past 4.5:1 and it still reads as red. */
  .ln.bad { color: color-mix(in srgb, var(--red) 85%, var(--text)); }
  .meta { grid-area: meta; display: flex; align-items: center; gap: 10px; }
  .cnt { min-width: 58px; text-align: right; white-space: nowrap; }
  /* The action slot keeps its width when a row has none, so every row's
     chip and count line up in one column. */
  .act { min-width: 118px; display: flex; justify-content: flex-end; }
  .list:not(:has(.act .btn)) .act { display: none; }
  .more { grid-area: more; width: 26px; justify-content: center; padding: 0; }
  .signin { border-top: 1px dashed var(--line); padding-left: 8px; }
  .mainrow { gap: 10px; }
  .msg { margin-top: 10px; }
  .msg.bad { color: var(--red); }
  .fp { margin-top: 8px; word-break: break-all; }
  .rmenu { position: fixed; z-index: 66; }
  .menu-item.bad { color: var(--red); }
  .empty-state :global(svg) { display: block; }
  @media (max-width: 600px) {
    .top { flex-wrap: wrap; }
    /* On a phone: the light, the name and the menu on top, the state under
       the name, then the chip, the count and the action. */
    .mrow { grid-template-columns: 8px minmax(0, 1fr) 26px; grid-template-areas: 'dot main more' '. meta meta'; row-gap: 6px; }
    .mrow > .dot { align-self: start; margin-top: 6px; }
    .cnt { min-width: 0; text-align: left; }
    .act { min-width: 0; }
    .meta { flex-wrap: wrap; }
  }
</style>
