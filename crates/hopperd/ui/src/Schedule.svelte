<script>
  import { CalendarClock, Save } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let { server } = $props();
  let s = $state(null);
  let info = $state(null);
  let warn = $state('');
  let saving = $state(false);

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);

  async function load() {
    try {
      info = await api(`${base}/schedule`);
      s = info.schedule;
      warn = s.warn_minutes.join(', ');
    } catch (e) {
      say(e.message, true);
    }
  }

  async function save() {
    saving = true;
    try {
      const body = {
        ...s,
        daily_restart: s.daily_restart || null,
        warn_minutes: warn.split(/[,\s]+/).filter(Boolean).map(Number).filter((n) => n > 0 && n < 1440),
        max_crash_restarts: Number(s.max_crash_restarts) || 0,
        backup_every_hours: Number(s.backup_every_hours) || 0,
        backup_keep: Math.max(1, Number(s.backup_keep) || 1),
      };
      await api(`${base}/schedule`, { method: 'PUT', body });
      say('Schedule saved');
      load();
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  $effect(() => {
    server.id;
    load();
  });
</script>

{#if s}
  <div class="card form">
    <h3><CalendarClock size={14} />Schedule<span class="sp"></span>
      <button class="btn sm pri" disabled={saving} onclick={save}><Save size={13} />Save</button></h3>

    <div class="sec lbl">Daily restart</div>
    <div class="f"><div><div>Restart every day at</div><div class="tiny muted">Local time on the server PC. Empty: no daily restart.</div></div>
      <div class="input w"><input type="time" bind:value={s.daily_restart} /></div></div>
    <div class="f"><div><div>Warn players before</div><div class="tiny muted">Minutes, comma separated. Sent as a chat message.</div></div>
      <div class="input w"><input bind:value={warn} placeholder="15, 5, 1" /></div></div>
    {#if info?.next_restart}<div class="tiny muted">{info.next_restart}</div>{/if}

    <div class="sec lbl">Crashes</div>
    <label class="f"><div><div>Start it again after a crash</div><div class="tiny muted">Stops trying after the limit within an hour.</div></div>
      <input type="checkbox" bind:checked={s.auto_restart_on_crash} /></label>
    <div class="f"><div>Crash restarts allowed per hour</div><div class="input w"><input type="number" min="0" max="20" bind:value={s.max_crash_restarts} /></div></div>
    {#if info?.gave_up}<div class="err small">{info.gave_up}</div>{/if}

    <div class="sec lbl">Backups</div>
    <div class="f"><div><div>Back up every</div><div class="tiny muted">Hours, while the server runs. 0: off.</div></div>
      <div class="input w"><input type="number" min="0" max="168" bind:value={s.backup_every_hours} /></div></div>
    <div class="f"><div>Scheduled backups kept</div><div class="input w"><input type="number" min="1" max="500" bind:value={s.backup_keep} /></div></div>
    <div class="f"><div>What they hold</div>
      <div class="seg">
        <button class:on={s.backup_scope === 'worlds'} onclick={() => (s.backup_scope = 'worlds')}>Worlds</button>
        <button class:on={s.backup_scope === 'full'} onclick={() => (s.backup_scope = 'full')}>Everything</button>
      </div></div>
    {#if info?.last_backup}<div class="tiny muted">Last scheduled backup {new Date(info.last_backup * 1000).toLocaleString()}</div>{/if}

    <div class="sec lbl">Start-up</div>
    <label class="f"><div><div>Start with the panel</div><div class="tiny muted">When hopperd starts (e.g. after the PC reboots).</div></div>
      <input type="checkbox" bind:checked={s.start_with_hopper} /></label>
  </div>
{/if}

<style>
  .form { max-width: 720px; display: flex; flex-direction: column; gap: 4px; }
  .sec { margin-top: 14px; }
  .f { display: flex; align-items: center; gap: 16px; justify-content: space-between; padding: 8px 0; border-bottom: 1px solid var(--line); }
  .f input[type='checkbox'] { width: 16px; height: 16px; accent-color: var(--acc); }
  .w { width: 140px; padding: 4px 8px; }
  .seg { display: flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; }
  .seg button { padding: 4px 12px; font-size: 12.5px; color: var(--subtle); }
  .seg button.on { background: var(--acc); color: #1a1a1f; font-weight: 600; }
</style>
