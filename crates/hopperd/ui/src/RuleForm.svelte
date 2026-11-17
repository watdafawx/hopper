<script>
  // The editor for every rule: the sentence it makes so far, name and servers,
  // then When / Only if / Then blocks that read top to bottom like it, then the
  // cooldown. "Then" is either a message (saved as a webhook; When = chips, a
  // message can wait for several events) or steps (saved as an automation;
  // When = one trigger). `locked`: a saved rule keeps what it is, so "Then"
  // can't change.
  import { Plus, X } from '@lucide/svelte';
  import { DAYS, LASTING, TIMED, STEPS, whenValue, triggerFor, upByDefault, stepValue, stepFor, heavy } from './lib/rules.js';
  import { switchThen, canSave, needsCooldown as cooldownShort, formSentence } from './lib/ruleForm.js';

  let { form = $bindable(), events, hookEvents = [], servers, minCooldown = 5, locked = false, onsave, oncancel } = $props();

  const flip = (list, v) => (list.includes(v) ? list.filter((x) => x !== v) : [...list, v].sort());
  /** Message events keep the order they were picked in; the sentence orders them itself. */
  const flipAny = (list, v) => (list.includes(v) ? list.filter((x) => x !== v) : [...list, v]);

  function setWhen(v) {
    form.trigger = triggerFor(v);
    form.only.running = upByDefault(form.trigger);
  }
  const setStep = (i, v) => (form.steps[i] = stepFor(v));
  const isMsg = $derived(form.then === 'message');
  const dailyRestart = $derived(form.trigger.kind === 'daily' && form.steps.some((s) => s.kind === 'power' && s.action === 'restart'));
  const needsCooldown = $derived(cooldownShort(form, heavy(form.steps), minCooldown));
  const saveOk = $derived(canSave(form, heavy(form.steps), minCooldown));
  const said = $derived(formSentence(form, hookEvents, events));
  const usesPlayer = $derived(form.trigger.kind === 'event' && ['join', 'leave'].includes(form.trigger.event));
  const hasWindow = $derived(form.only.from != null || form.only.to != null);
</script>

<form class="col" onsubmit={(e) => { e.preventDefault(); onsave(); }}>
  <p class="said">{#if said.when.startsWith('When ')}When <b>{said.when.slice(5)}</b>{:else}<b>{said.when}</b>{/if}, <b>{said.then || '…'}</b></p>
  <div class="top">
    <div class="input name"><input bind:value={form.name} placeholder={isMsg ? 'Name (optional), like “Team Discord”' : 'Name, like “Restart when it lags”'} required={!isMsg} aria-label="Name" /></div>
    {#if servers.length > 1}
      <div class="chips">
        <span class="tiny muted">Servers{form.servers.length ? '' : ' (all)'}</span>
        {#each servers as s (s.id)}
          <button type="button" class="chip" class:acc={form.servers.includes(s.id)} onclick={() => (form.servers = flip(form.servers, s.id))}>{s.name}</button>
        {/each}
      </div>
    {/if}
  </div>

  <section class="blk">
    <span class="lbl">When</span>
    {#if isMsg}
      <div class="chips">
        {#each hookEvents as ev (ev.id)}
          <button type="button" class="chip" class:acc={form.events.includes(ev.id)} aria-pressed={form.events.includes(ev.id)} onclick={() => (form.events = flipAny(form.events, ev.id))}>{ev.label}</button>
        {/each}
      </div>
      <div class="tiny muted">A message can wait for several events. For TPS, player counts or a time of day, choose “Run steps” below.</div>
    {:else}
      <div class="line">
        <select class="sel" value={whenValue(form.trigger)} onchange={(e) => setWhen(e.target.value)}>
          <optgroup label="Something happens">{#each events as ev (ev.id)}<option value={ev.id}>{ev.label}</option>{/each}</optgroup>
          <optgroup label="Something lasts">{#each LASTING as [v, l] (v)}<option value={v}>{l}</option>{/each}</optgroup>
          <optgroup label="At a time">{#each TIMED as [v, l] (v)}<option value={v}>{l}</option>{/each}</optgroup>
        </select>
        {#if form.trigger.kind === 'tps'}
          <div class="input n"><input type="number" min="1" max="20" step="0.5" bind:value={form.trigger.below} /></div> TPS for
          <div class="input n"><input type="number" min="1" max="120" bind:value={form.trigger.minutes} /></div> min
        {:else if form.trigger.kind === 'players'}
          <select class="sel" bind:value={form.trigger.at_most}><option value={true}>at most</option><option value={false}>at least</option></select>
          <div class="input n"><input type="number" min="0" bind:value={form.trigger.count} /></div> players for
          <div class="input n"><input type="number" min="1" max="1440" bind:value={form.trigger.minutes} /></div> min
        {:else if form.trigger.kind === 'every'}
          <div class="input n"><input type="number" min="1" bind:value={form.trigger.minutes} /></div> minutes
        {:else if form.trigger.kind === 'daily'}
          <div class="input t"><input type="time" bind:value={form.trigger.at} /></div>
        {/if}
      </div>
      {#if form.trigger.kind === 'tps'}<div class="tiny muted">Servers with RCON only, as for the low TPS warning.</div>{/if}
    {/if}
  </section>

  {#if !isMsg}
    <section class="blk">
      <span class="lbl">Only if <span class="opt">(optional)</span></span>
      <label class="line"><input type="checkbox" bind:checked={form.only.running} /> the server is up</label>
      <div class="line">Players online from
        <div class="input n"><input type="number" min="0" placeholder="any" bind:value={form.only.players_min} /></div> to
        <div class="input n"><input type="number" min="0" placeholder="any" bind:value={form.only.players_max} /></div></div>
      <div class="line">
        {#if hasWindow}
          Between <div class="input t"><input type="time" bind:value={form.only.from} required /></div> and
          <div class="input t"><input type="time" bind:value={form.only.to} required /></div>
          <button type="button" class="btn sm icon ghost" title="Any time" onclick={() => { form.only.from = null; form.only.to = null; }}><X size={13} /></button>
        {:else}
          <button type="button" class="btn sm ghost" onclick={() => { form.only.from = '22:00'; form.only.to = '06:00'; }}><Plus size={13} />Time window</button>
        {/if}
        <span class="sp"></span>
        {#each DAYS as d, i (d)}
          <button type="button" class="chip" class:acc={!form.only.days.length || form.only.days.includes(i)} onclick={() => {
            const all = form.only.days.length ? form.only.days : [0, 1, 2, 3, 4, 5, 6];
            const next = flip(all, i);
            form.only.days = next.length === 7 ? [] : next;
          }}>{d}</button>
        {/each}
      </div>
    </section>
  {/if}

  <section class="blk">
    <span class="lbl">Then</span>
    <div class="seg icons pick" role="group" aria-label="Then">
      <button type="button" class="si" aria-pressed={isMsg} disabled={locked && !isMsg} onclick={() => switchThen(form, 'message', events.map((e) => e.id))}>Post a message</button>
      <button type="button" class="si" aria-pressed={!isMsg} disabled={locked && isMsg} onclick={() => switchThen(form, 'steps', events.map((e) => e.id))}>Run steps</button>
    </div>
    {#if isMsg}
      <div class="input"><input bind:value={form.url} placeholder="https://discord.com/api/webhooks/…" required aria-label="Discord or other URL" /></div>
      <div class="tiny muted">Discord URLs get Discord messages; any other URL gets JSON: event, server, text, at.</div>
    {:else}
      {#each form.steps as s, i (i)}
        <div class="act">
          <span class="tiny muted num">{i + 1}</span>
          <select class="sel" value={stepValue(s)} onchange={(e) => setStep(i, e.target.value)}>
            {#each STEPS as [v, l] (v)}<option value={v}>{l}</option>{/each}
          </select>
          {#if s.kind === 'command'}
            <div class="input grow"><input class="mono" bind:value={s.line} placeholder="say Hello {'{player}'}" required /></div>
          {:else if s.kind === 'backup'}
            <select class="sel" bind:value={s.worlds_only}><option value={true}>worlds only</option><option value={false}>everything</option></select>
          {:else if s.kind === 'url'}
            <div class="input grow"><input bind:value={s.url} placeholder="https://…" required /></div>
          {:else if s.kind === 'wait'}
            <div class="input n"><input type="number" min="1" max="3600" bind:value={s.seconds} /></div> seconds
          {/if}
          {#if s.kind !== 'command' && s.kind !== 'url'}<span class="sp"></span>{/if}
          <button type="button" class="btn sm icon ghost" title="Remove this step" disabled={form.steps.length === 1} onclick={() => form.steps.splice(i, 1)}><X size={13} /></button>
        </div>
      {/each}
      {#if form.steps.length < 10}
        <div><button type="button" class="btn sm ghost" onclick={() => form.steps.push(stepFor('command'))}><Plus size={13} />Add a step</button></div>
      {/if}
      <div class="tiny muted">In commands, {'{server}'} is the server's name{usesPlayer ? ' and {player} the player who joined or left' : ''}. A URL gets JSON: automation, event, server, player, text, at. Backups count as scheduled ones (the server's Schedule says how many are kept).</div>
      {#if dailyRestart}<div class="tiny warn">For a daily restart that warns players first, use the server's Schedule instead.</div>{/if}
    {/if}
  </section>

  <div class="line">
    {#if !isMsg}At most once every <div class="input n"><input type="number" min="0" bind:value={form.cooldown_minutes} /></div> min per server{/if}
    <span class="sp"></span>
    <button type="button" class="btn sm" onclick={oncancel}>Cancel</button>
    <button class="btn sm pri" disabled={!saveOk}>Save</button>
  </div>
  {#if needsCooldown}<div class="tiny warn">Rules that start, stop, restart or back up wait at least {minCooldown} minutes between runs.</div>{/if}
</form>

<style>
  .said { font-size: 14px; color: var(--subtle); overflow-wrap: anywhere; }
  .said b { color: var(--text); font-weight: 600; }
  .pick { align-self: flex-start; }
  .pick .si:disabled { opacity: 0.45; cursor: default; }
  .top { display: flex; flex-direction: column; gap: 8px; }
  .chips { display: flex; flex-wrap: wrap; gap: 5px; align-items: center; }
  .chip { cursor: pointer; }
  .blk { border: 1px solid var(--line); border-radius: var(--radius-lg); padding: 10px; display: flex; flex-direction: column; gap: 8px; }
  .opt { text-transform: none; letter-spacing: 0; font-weight: 400; }
  .line { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; font-size: 12.5px; }
  .act { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; background: var(--base); border-radius: var(--radius); padding: 5px 8px; font-size: 12.5px; }
  .num { width: 12px; text-align: right; }
  .input { padding: 4px 8px; }
  .input.n { width: 76px; }
  .input.t { width: 110px; }
  .input.grow { flex: 1; min-width: 180px; }
  .sel { background: var(--base); border: 1px solid var(--input-line); border-radius: var(--radius); padding: 4px 8px; color: var(--text); font: inherit; font-size: 12.5px; }
  .warn { color: var(--gold); }
  input[type='checkbox'] { accent-color: var(--acc); }
</style>
