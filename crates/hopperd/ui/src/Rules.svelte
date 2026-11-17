<script>
  // One list of what the panel tells you and does on its own, every row a
  // "When X, then Y" sentence. Three stores sit behind it and keep their own
  // routes: messages (/webhooks), warnings (/alerts/settings, the five built-in
  // ones) and actions (/rules). This page merges them (lib/rulesList.js) and
  // sends each add, edit, switch and remove to the store the row came from.
  import { onMount } from 'svelte';
  import { Plus, Search, X, AlertTriangle, ListChecks } from '@lucide/svelte';
  import { onChange } from './lib/changes.svelte.js';
  import { api, post, del } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { alerts, refreshAlerts } from './lib/alerts.svelte.js';
  import { live } from './lib/live.svelte.js';
  import { rightClear } from './lib/rightclear.js';
  import { pick, startPick, swept, sweeping, clickSwallowed, countLabel } from './lib/dragpick.svelte.js';
  import { FILTERS } from './lib/rulesPage.js';
  import { buildItems, visibleItems, countItems, failingItems, settingsMatch } from './lib/rulesList.js';
  import { blankForm, formFromHook, formFromRule, hookBody, ruleBody } from './lib/ruleForm.js';
  import RuleForm from './RuleForm.svelte';
  import RuleRow from './RuleRow.svelte';

  // `start`: the filter an old link (Settings › Alerts) opens on.
  let { start = 'all' } = $props();
  let filter = $state(start);
  let query = $state('');
  let hooks = $state([]);
  let hookEvents = $state([]);
  let rules = $state([]);
  let ruleEvents = $state([]);
  let minCooldown = $state(5);
  let servers = $state([]);
  let ready = $state(false);
  let warnFailed = $state(false);
  /** The open editor: `at` is 'new' or the row it replaces. */
  let editing = $state(null);
  /** The action whose "Run now" is asking which server. */
  let picking = $state(null);
  let open = $state({});

  const hasNodes = $derived((live.nodes || []).length > 0);
  const items = $derived(buildItems({ hooks, hookEvents, rules, ruleEvents, settings: alerts.settings, hasNodes, servers }));
  const n = $derived(countItems(items));
  const bad = $derived(failingItems(items));
  const shown = $derived(visibleItems(items, filter, query));
  const showSettings = $derived((filter === 'all' || filter === 'warnings') && !!alerts.settings && settingsMatch(query, true));
  const multi = $derived(servers.length > 1);

  async function loadHooks() {
    try {
      const r = await api('/webhooks');
      hooks = r.hooks;
      hookEvents = r.events;
    } catch (e) {
      say(e.message, true);
    }
  }
  async function loadRules() {
    try {
      const r = await api('/rules');
      rules = r.rules;
      ruleEvents = r.events;
      minCooldown = r.min_power_cooldown ?? 5;
    } catch (e) {
      say(e.message, true);
    }
  }
  async function loadAll() {
    await Promise.all([loadHooks(), loadRules(), refreshAlerts()]);
    ready = true;
    warnFailed = !alerts.settings;
  }

  // ---- add and edit ----

  function addRule() {
    editing = { at: 'new', form: blankForm(filter === 'actions' ? 'steps' : 'message') };
  }
  function edit(item) {
    editing = { at: item.key, form: item.kind === 'message' ? formFromHook(item.src) : formFromRule(item.src) };
  }

  async function save() {
    const f = editing.form;
    try {
      if (f.then === 'message') {
        if (f.id) await api(`/webhooks/${f.id}`, { method: 'PUT', body: hookBody(f) });
        else await post('/webhooks', hookBody(f));
        say(f.name ? `Saved ${f.name}` : 'Saved the message');
      } else {
        if (f.id) await api(`/rules/${f.id}`, { method: 'PUT', body: ruleBody(f) });
        else await post('/rules', ruleBody(f));
        say(`Saved ${f.name}`);
      }
      editing = null;
      f.then === 'message' ? loadHooks() : loadRules();
    } catch (e) {
      say(e.message, true);
    }
  }

  // ---- switch, remove ----

  /** The warnings' settings go back whole, numbers as numbers (as the old Alerts card did). */
  async function saveSettings(patch) {
    const s = { ...alerts.settings, ...patch };
    const body = { ...s, tps_below: Number(s.tps_below), disk_gb: Number(s.disk_gb), ram_pct: Number(s.ram_pct), tps_minutes: Number(s.tps_minutes), ram_minutes: Number(s.ram_minutes), node_minutes: Number(s.node_minutes) };
    try {
      const r = await api('/alerts/settings', { method: 'PUT', body });
      alerts.settings = { ...r.settings };
      return true;
    } catch (e) {
      say(e.message, true);
      refreshAlerts();
      return false;
    }
  }

  async function setOn(item, on) {
    try {
      if (item.kind === 'warning') return await saveSettings({ [item.flag]: on });
      const path = item.kind === 'message' ? `/webhooks/${item.id}` : `/rules/${item.id}`;
      await api(path, { method: 'PUT', body: { ...item.src, enabled: on } });
      return true;
    } catch (e) {
      say(e.message, true);
      return false;
    }
  }
  async function toggle(item, e) {
    if (clickSwallowed()) return e.preventDefault();
    const box = e.currentTarget;
    // A refused change puts the switch back.
    if (!(await setOn(item, !item.enabled))) box.checked = item.enabled;
    item.kind === 'message' ? loadHooks() : item.kind === 'action' ? loadRules() : null;
  }

  async function remove(item) {
    if (!(await confirmIt(`Remove ${item.name || 'this message'}?`, { ok: 'Remove', danger: true }))) return;
    try {
      await del(`${item.kind === 'message' ? '/webhooks' : '/rules'}/${item.id}`);
      item.kind === 'message' ? loadHooks() : loadRules();
    } catch (e) {
      say(e.message, true);
    }
  }

  /** A warning's number, edited in its sentence. `null`: it was out of range, the box already went back. */
  async function setNumber(p, v) {
    if (v == null) return say(`Pick a number from ${p.min} to ${p.max}.`, true);
    if (await saveSettings({ [p.field]: v })) say('Warning saved');
  }

  // ---- drag across switches: every switch the pointer crosses takes the same state ----

  const swSweep = $derived(sweeping('rules'));
  const isOn = (item, ix) => (swSweep === 'switch' && swept('rules', ix) ? pick.on : item.enabled);
  function switchDown(e, item, ix) {
    const on = !item.enabled;
    startPick(e, {
      list: 'rules', kind: 'switch', ix, on,
      keyAt: (i) => (shown[i] && shown[i].enabled !== on ? shown[i].key : null),
      label: countLabel(on ? 'Turning on' : 'Turning off', on ? 'All on' : 'All off'),
      onCommit: async (keys) => {
        for (const k of keys) {
          const it = shown.find((x) => x.key === k);
          if (it) await setOn(it, on);
        }
        loadAll();
      },
    });
  }

  // ---- messages: send a test; actions: run now ----

  async function test(item) {
    try {
      await post(`/webhooks/${item.id}/test`);
      say(`Sent a test to ${item.name || 'the message'}`);
    } catch (e) {
      say(e.message, true);
    }
  }

  const covered = (item) => (item.servers.length ? servers.filter((s) => item.servers.includes(s.id)) : servers);
  function runNow(item) {
    const list = covered(item);
    if (list.length === 1) run(item, list[0]);
    else picking = picking === item.key ? null : item.key;
  }
  async function run(item, s) {
    picking = null;
    const r = item.src;
    const power = r.steps.find((x) => x.kind === 'power');
    if (power && !(await confirmIt(`Run ${r.name} on ${s.name} now? It will ${power.action} the server.`, { ok: 'Run now' }))) return;
    try {
      const res = await post(`/rules/${r.id}/run`, { server: s.id });
      if (res.started) say(`${r.name} is running on ${s.name}`);
      else if (res.run?.ok) say(`${r.name} ran on ${s.name}: ${res.run.text}`);
      else say(res.run?.text || 'It failed', true);
      loadRules();
    } catch (e) {
      say(e.message, true);
    }
  }

  onMount(async () => {
    loadAll();
    try {
      servers = await api('/servers');
    } catch {}
  });
  // Changed elsewhere (another tab, Hopper desktop, on disk): show it.
  $effect(() => onChange(null, ['panel'], loadAll));
  // A different old link while the page is open.
  $effect(() => {
    filter = start;
  });
</script>

<div class="rbar">
  <div class="seg icons" role="group" aria-label="Show">
    {#each FILTERS as [k, label] (k)}
      <button class="si" aria-pressed={filter === k} onclick={() => (filter = k)}>{label}<span class="ct">{n[k]}</span></button>
    {/each}
  </div>
  <div class="input find"><Search size={13} /><input use:rightClear placeholder="Search" bind:value={query} spellcheck="false" aria-label="Search rules" />{#if query}<button class="clr" title="Clear" aria-label="Clear" onclick={() => (query = '')}><X size={12} /></button>{/if}</div>
  <span class="sp"></span>
  <button class="btn sm pri" onclick={addRule}><Plus size={13} />Add rule</button>
</div>
{#if bad}<p class="tiny warn"><AlertTriangle size={12} />{bad === 1 ? '1 rule needs a look' : `${bad} rules need a look`}: it failed the last time it ran.</p>{/if}

<div class="card list">
  {#if editing?.at === 'new'}
    <div class="ed"><RuleForm bind:form={editing.form} events={ruleEvents} {hookEvents} {servers} {minCooldown} onsave={save} oncancel={() => (editing = null)} /></div>
  {/if}
  {#if !ready}
    <div class="skel"><i></i></div><div class="skel"><i></i></div><div class="skel"><i></i></div>
  {:else}
    <div data-pick-list="rules">
      {#each shown as item, ix (item.key)}
        {#if editing?.at === item.key}
          <div class="ed" data-pick-ix={ix}><RuleForm bind:form={editing.form} locked events={ruleEvents} {hookEvents} {servers} {minCooldown} onsave={save} oncancel={() => (editing = null)} /></div>
        {:else}
          <RuleRow {item} {ix} {multi} on={isOn(item, ix)} pending={isOn(item, ix) !== item.enabled} open={!!open[item.key]} picking={picking === item.key}
            covered={item.kind === 'action' ? covered(item) : []}
            ontoggle={(e) => toggle(item, e)} onswitchdown={(e) => switchDown(e, item, ix)} ontest={() => test(item)}
            onrun={() => runNow(item)} onrunon={(s) => run(item, s)} onedit={() => edit(item)} onremove={() => remove(item)}
            onnum={setNumber} onlog={() => (open[item.key] = !open[item.key])} />
        {/if}
      {/each}
    </div>
    {#if !shown.length && editing?.at !== 'new'}
      {#if query || items.length}
        <p class="small subtle none">No rules match{query ? ` “${query}”` : ' this filter'}.</p>
      {:else}
        <div class="empty-state">
          <span class="es-ic"><ListChecks size={18} /></span>
          <b>No rules yet</b>
          <p>When something happens, tell someone or do something: post to Discord when a server crashes, restart it when TPS stays low, back up every night.</p>
        </div>
      {/if}
    {/if}
  {/if}
</div>

{#if warnFailed && (filter === 'all' || filter === 'warnings')}<p class="small muted">Couldn't load the warnings. Open this page again to retry.</p>{/if}

{#if showSettings}
  <div class="card sets">
    <h3>Warning settings</h3>
    <div class="f"><div class="l">Turn on RCON for all servers automatically<small>New servers always get RCON; this turns it on for the ones you had before too, so TPS, ping and console commands work everywhere. Each gets its own port and a random password; RCON you set up yourself is kept. It starts with each server's next restart. The panel opens no firewall ports for it.</small></div>
      <label class="switch" title={alerts.settings.auto_rcon ? 'Turn off' : 'Turn on'}><input type="checkbox" checked={alerts.settings.auto_rcon} aria-label="Turn on RCON for all servers automatically" onchange={(e) => saveSettings({ auto_rcon: e.target.checked })} /><span></span></label></div>
    <p class="tiny muted">Each warning fires once, then again only after it has been clear for 10 minutes. Pick “Low TPS”, “Drive nearly full” and “PC nearly out of memory” in a message rule to get them on Discord.</p>
  </div>
{/if}

{#if filter === 'all' || filter === 'actions'}
  <p class="tiny muted foot">Crash restarts, the daily restart and scheduled backups are on each server's Schedule. Rules can't run programs on this PC; point “Call a URL” at a local service to do that.</p>
{/if}

<style>
  .rbar { flex: none; display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-bottom: 12px; }
  .find { width: 180px; flex: 0 1 180px; min-width: 120px; }
  .si .ct { margin-left: 5px; color: var(--muted); font-size: 11px; }
  .warn { flex: none; display: flex; align-items: center; gap: 5px; color: var(--red-text); margin: -2px 0 10px; }
  .list { padding: 4px 10px; }
  .list :global(.rule:last-child) { border-bottom: 0; }
  .ed { padding: 10px 0; border-bottom: 1px solid var(--line); }
  .none { padding: 12px 4px; }
  .sets { margin-top: 12px; }
  .f { display: flex; align-items: center; gap: 10px; padding: 7px 0; }
  .l { flex: 1; font-size: 12.5px; }
  .l small { display: block; color: var(--muted); font-size: 11px; }
  .sets p { margin-top: 8px; }
  .foot { margin-top: 10px; }
</style>
