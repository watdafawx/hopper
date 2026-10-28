<script>
  // A server's schedule, as desktop's Server settings › Schedule: daily
  // restart with warnings, start again after a crash, start with the panel,
  // plus scheduled backups. Save and Undo changes only light up once
  // something changed.
  import { CalendarClock, Save, RotateCcw } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { fullDate } from './lib/worlds.js';

  let { server } = $props();
  // The live feed hands over a new `server` object every second; effects
  // that should run once per server key on the id alone.
  const sid = $derived(server.id);
  let s = $state(null);
  let saved = $state('');
  let info = $state(null);
  let warn = $state('');
  let saving = $state(false);

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);

  // Whole numbers only: the panel stores them as such (2.5 would be refused).
  const int = (v, lo, hi) => Math.min(hi, Math.max(lo, Math.round(Number(v)) || 0));
  const body = () => ({
    ...s,
    daily_restart: s.daily_restart || null,
    // As the panel saves them: newest warning first, no repeats.
    warn_minutes: [...new Set(warn.split(/[,\s]+/).filter(Boolean).map((n) => Math.round(Number(n))).filter((n) => n > 0 && n < 1440))].sort((a, b) => b - a),
    max_crash_restarts: int(s.max_crash_restarts, 0, 20),
    backup_every_hours: int(s.backup_every_hours, 0, 168),
    backup_keep: Math.max(1, int(s.backup_keep, 1, 500)),
  });
  const changed = $derived(!!s && JSON.stringify(body()) !== saved);

  async function load() {
    try {
      info = await api(`${base}/schedule`);
      s = info.schedule;
      warn = s.warn_minutes.join(', ');
      saved = JSON.stringify(body());
    } catch (e) {
      say(e.message, true);
    }
  }

  async function save() {
    if (!changed) return;
    saving = true;
    try {
      await api(`${base}/schedule`, { method: 'PUT', body: body() });
      say('Schedule saved');
      load();
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  $effect(() => {
    sid;
    load();
  });

  // Ctrl+S saves, as in the panel's editors.
  function keys(e) {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's' && changed) {
      e.preventDefault();
      save();
    }
  }
</script>

<svelte:window onkeydown={keys} />

{#if s}
  <div class="card form">
    <h3><CalendarClock size={14} />Schedule</h3>

    <div class="sec lbl">Restarts</div>
    <div class="f"><div><div>Daily restart</div><div class="tiny muted">{info?.next_restart ? `Next: ${info.next_restart}. Players get warnings in chat first.` : 'A time (HH:MM, the server PC\'s time) to restart every day, or empty for none.'}</div></div>
      <div class="input w"><input type="time" bind:value={s.daily_restart} /></div></div>
    <div class="f"><div><div>Warn before</div><div class="tiny muted">Minutes before the restart to warn players.</div></div>
      <div class="input w"><input bind:value={warn} placeholder="15, 5, 1" /></div></div>

    <div class="f"><div><div>After a crash</div>{#if info?.gave_up}<div class="err small">{info.gave_up}</div>{/if}</div>
      <div class="row">
        <label class="switch"><input type="checkbox" bind:checked={s.auto_restart_on_crash} aria-label="Start it again after a crash" /><span></span></label>
        <span class="small">Start it again, up to</span>
        <div class="input n"><input type="number" min="0" max="20" bind:value={s.max_crash_restarts} disabled={!s.auto_restart_on_crash} /></div>
        <span class="small">times an hour</span>
      </div></div>

    <div class="f"><div>With the panel</div>
      <label class="row"><span class="switch"><input type="checkbox" bind:checked={s.start_with_hopper} /><span></span></span><span class="small">Start this server when the panel starts</span></label></div>

    {#if server.sleep_after_minutes}
      <div class="f"><div><div>Sleep</div><div class="tiny muted">Stops after {server.sleep_after_minutes} min with nobody on, and starts when someone joins.</div></div>
        <a class="small" href="#/server/{encodeURIComponent(server.id)}/settings">Settings › Address</a></div>
    {/if}

    <div class="sec lbl">Backups</div>
    <div class="f"><div><div>Back up every</div><div class="tiny muted">Hours, while the server runs. 0: off.{info?.last_backup ? ` Last one ${fullDate(info.last_backup)}.` : ''}</div></div>
      <div class="input w"><input type="number" min="0" max="168" bind:value={s.backup_every_hours} /></div></div>
    <div class="f"><div>What they hold</div>
      <div class="seg">
        <button class:on={s.backup_scope === 'worlds'} onclick={() => (s.backup_scope = 'worlds')}>Worlds</button>
        <button class:on={s.backup_scope === 'full'} onclick={() => (s.backup_scope = 'full')}>Everything</button>
      </div></div>
    <div class="f"><div><div>Which ones are kept</div><div class="tiny muted">Set in the Backups tab: the newest {s.backup_keep}, and one a day or a week if you like.</div></div></div>

    <div class="row foot">
      <button class="btn pri" disabled={!changed || saving} onclick={save}><Save size={13} />Save</button>
      <button class="btn ghost" disabled={!changed || saving} onclick={load}><RotateCcw size={13} />Undo changes</button>
    </div>
  </div>
{/if}

<style>
  .form { max-width: 720px; display: flex; flex-direction: column; gap: 4px; }
  .sec { margin-top: 14px; }
  .f { display: flex; align-items: center; gap: 16px; justify-content: space-between; padding: 8px 0; border-bottom: 1px solid var(--line); }
  .w { width: 140px; padding: 4px 8px; }
  .n { width: 64px; padding: 4px 8px; }
  .n input:disabled { opacity: 0.5; }
  .seg { display: flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; }
  .seg button { padding: 4px 12px; font-size: 12.5px; color: var(--subtle); }
  .seg button.on { background: var(--acc); color: #1a1a1f; font-weight: 600; }
  .foot { margin-top: 14px; }
</style>
