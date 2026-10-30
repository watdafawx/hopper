<script>
  // Automations, beside Webhooks and Alerts: "when server X does Y, do Z".
  // Same shape as Webhooks: a list, an inline form, a switch, Run now.
  import { onMount } from 'svelte';
  import { Zap, Plus, Trash2, Play, Pencil, AlertTriangle, CheckCircle2 } from '@lucide/svelte';
  import { api, post, del } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { describe, since, stepFor } from './lib/rules.js';
  import RuleForm from './RuleForm.svelte';

  let rules = $state([]);
  let events = $state([]);
  let minCooldown = $state(5);
  let servers = $state([]);
  let loaded = $state(false);
  let form = $state(null);
  /** The rule whose "Run now" is asking which server. */
  let picking = $state(null);
  let open = $state({});

  async function load() {
    try {
      const r = await api('/rules');
      rules = r.rules;
      events = r.events;
      minCooldown = r.min_power_cooldown ?? 5;
    } catch (e) {
      say(e.message, true);
    }
    loaded = true;
  }

  function edit(r) {
    const copy = (x) => JSON.parse(JSON.stringify(x));
    form = r
      ? { id: r.id, name: r.name, enabled: r.enabled, servers: [...r.servers], trigger: copy(r.trigger), only: { days: [], ...copy(r.only) }, steps: copy(r.steps), cooldown_minutes: r.cooldown_minutes }
      : { name: '', enabled: true, servers: [], trigger: { kind: 'event', event: 'join' }, only: { running: true, players_min: null, players_max: null, from: null, to: null, days: [] }, steps: [stepFor('command')], cooldown_minutes: 0 };
  }

  /** The form's numbers as numbers, blanks as none. */
  function body(f) {
    const n = (v) => (v === '' || v == null ? null : Number(v));
    const t = { ...f.trigger };
    for (const k of ['below', 'minutes', 'count']) if (k in t) t[k] = Number(t[k]);
    const steps = f.steps.map((s) => (s.kind === 'wait' ? { ...s, seconds: Number(s.seconds) } : s));
    const only = { ...f.only, players_min: n(f.only.players_min), players_max: n(f.only.players_max), from: f.only.from || null, to: f.only.to || null };
    return { name: f.name, enabled: f.enabled, servers: f.servers, trigger: t, only, steps, cooldown_minutes: Number(f.cooldown_minutes) || 0 };
  }

  async function save() {
    try {
      if (form.id) await api(`/rules/${form.id}`, { method: 'PUT', body: body(form) });
      else await post('/rules', body(form));
      say(`Saved ${form.name}`);
      form = null;
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function toggle(r) {
    try {
      await api(`/rules/${r.id}`, { method: 'PUT', body: { ...r, enabled: !r.enabled } });
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function remove(r) {
    if (!(await confirmIt(`Remove ${r.name}?`, { ok: 'Remove', danger: true }))) return;
    try {
      await del(`/rules/${r.id}`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  const covered = (r) => (r.servers.length ? servers.filter((s) => r.servers.includes(s.id)) : servers);

  function runNow(r) {
    const list = covered(r);
    if (list.length === 1) run(r, list[0]);
    else picking = picking === r.id ? null : r.id;
  }

  async function run(r, s) {
    picking = null;
    const power = r.steps.find((x) => x.kind === 'power');
    if (power && !(await confirmIt(`Run ${r.name} on ${s.name} now? It will ${power.action} the server.`, { ok: 'Run now' }))) return;
    try {
      const res = await post(`/rules/${r.id}/run`, { server: s.id });
      if (res.started) say(`${r.name} is running on ${s.name}`);
      else if (res.run?.ok) say(`${r.name} ran on ${s.name}: ${res.run.text}`);
      else say(res.run?.text || 'It failed', true);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  const serverLabel = (r) => (r.servers.length === 0 ? 'All servers' : r.servers.length === 1 ? servers.find((s) => s.id === r.servers[0])?.name || r.servers[0] : `${r.servers.length} servers`);

  onMount(async () => {
    load();
    try {
      servers = await api('/servers');
    } catch {}
  });
</script>

<div class="card">
  <h3><Zap size={14} />Automations <span class="sp"></span>
    {#if !form}<button class="btn sm" onclick={() => edit(null)}><Plus size={13} />Add</button>{/if}</h3>
  {#if form}
    <RuleForm bind:form {events} {servers} {minCooldown} onsave={save} oncancel={() => (form = null)} />
  {:else if !loaded}
    <div class="skel"><i></i></div><div class="skel"><i></i></div>
  {:else}
    {#each rules as r (r.id)}
      {@const last = r.history[0]}
      <div class="rule" class:off={!r.enabled}>
        <div class="grow">
          <div class="row">{r.name} <span class="chip">{serverLabel(r)}</span></div>
          <div class="tiny muted">{describe(r, events)}</div>
          {#if last}
            <button class="tiny last" class:bad={!last.ok} onclick={() => (open[r.id] = !open[r.id])} title="Last runs">
              {#if last.ok}<CheckCircle2 size={11} />{:else}<AlertTriangle size={11} />{/if}
              {last.ok ? 'Ran' : 'Failed'} {since(last.at)}{servers.length > 1 ? ` on ${last.server}` : ''}{last.manual ? ' (by hand)' : ''}{last.ok ? '' : `: ${last.text}`}
            </button>
            {#if open[r.id]}
              <div class="log">
                {#each r.history as h, i (i)}
                  <div class:bad={!h.ok}>{new Date(h.at * 1000).toLocaleString()} · {h.server} · {h.text}{h.manual ? ' (by hand)' : ''}</div>
                {/each}
              </div>
            {/if}
          {:else}
            <div class="tiny subtle">Hasn't run yet</div>
          {/if}
          {#if picking === r.id}
            <div class="pick"><span class="tiny muted">Run on</span>
              {#each covered(r) as s (s.id)}<button class="chip acc" onclick={() => run(r, s)}>{s.name}</button>{/each}
            </div>
          {/if}
        </div>
        <label class="switch" title={r.enabled ? 'Turn off' : 'Turn on'}><input type="checkbox" checked={r.enabled} onchange={() => toggle(r)} /><span></span></label>
        <button class="btn sm icon ghost" title="Run now" onclick={() => runNow(r)}><Play size={13} /></button>
        <button class="btn sm icon ghost" title="Edit" onclick={() => edit(r)}><Pencil size={13} /></button>
        <button class="btn sm icon ghost" title="Remove" onclick={() => remove(r)}><Trash2 size={13} /></button>
      </div>
    {:else}
      <p class="small subtle">Have the panel act on its own: restart a server when TPS stays low, greet players who join, back up every night, or call a URL when a server crashes.</p>
    {/each}
    <p class="tiny muted foot">Crash restarts, the daily restart and scheduled backups are on each server's Schedule. Automations can't run programs on this PC; point "Call a URL" at a local service to do that.</p>
  {/if}
</div>

<style>
  .rule { display: flex; align-items: center; gap: 8px; padding: 9px 0; border-bottom: 1px solid var(--line); }
  .rule:last-of-type { border: 0; }
  .rule.off { opacity: 0.55; }
  .grow { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
  .last { display: inline-flex; align-items: center; gap: 4px; color: var(--green); align-self: flex-start; text-align: left; }
  .last.bad { color: var(--red); }
  .log { font: 11px/1.6 var(--mono); color: var(--subtle); background: var(--term); border-radius: var(--radius); padding: 6px 8px; margin-top: 4px; max-height: 160px; overflow-y: auto; }
  .log .bad { color: var(--red); }
  .pick { display: flex; flex-wrap: wrap; gap: 5px; align-items: center; margin-top: 4px; }
  .pick .chip { cursor: pointer; }
  .foot { margin-top: 10px; }
</style>
