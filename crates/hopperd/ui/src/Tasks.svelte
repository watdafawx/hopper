<script>
  // The title bar's Tasks button and its popover, like Hopper desktop's
  // (src/gpui/tasks.rs): the panel's background jobs (pack updates,
  // comparisons) with a spinner, a bar, Cancel while one runs and Dismiss
  // after. Owner only: the list names servers others may not see.
  import { onMount, onDestroy } from 'svelte';
  import { ListChecks, CircleCheck, CircleX, Copy } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let jobs = $state([]);
  let open = $state(false);
  let gone = $state(false);
  let wrap = $state();
  let timer;
  let dead = false;

  const running = $derived(jobs.filter((j) => !j.done).length);
  const failed = $derived(jobs.filter((j) => j.done && j.error).length);

  async function load() {
    try {
      jobs = await api('/jobs');
    } catch (e) {
      // An older panel, or not the owner: no task list.
      if (e.status === 404 || e.status === 403) gone = true;
    }
  }

  // Every second while open or something runs, else every few seconds for
  // the button's spinner; nothing while the tab is hidden (it looks again
  // when it shows).
  function schedule() {
    clearTimeout(timer);
    if (dead || gone || document.hidden) return;
    timer = setTimeout(async () => {
      await load();
      schedule();
    }, open || running ? 1000 : 5000);
  }

  function visible() {
    if (document.hidden) clearTimeout(timer);
    else load().then(schedule);
  }

  async function act(j) {
    try {
      if (j.done) await post(`/jobs/${j.id}/dismiss`);
      else await post(`/jobs/${j.id}/cancel`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function clearFinished() {
    try {
      await post('/jobs/clear-finished');
      await load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function copy(j) {
    try {
      await navigator.clipboard.writeText(`${j.title}: ${j.error}`);
      say('Copied.');
    } catch {
      say("Can't copy here.", true);
    }
  }

  function outside(e) {
    if (open && wrap && !wrap.contains(e.target)) open = false;
  }

  function esc(e) {
    if (open && e.key === 'Escape') {
      open = false;
      wrap?.querySelector('.tb')?.focus();
    }
  }

  function toggle() {
    open = !open;
    if (open) load().then(schedule);
  }

  onMount(() => {
    load().then(schedule);
    addEventListener('pointerdown', outside);
    addEventListener('keydown', esc);
    document.addEventListener('visibilitychange', visible);
  });
  onDestroy(() => {
    dead = true;
    clearTimeout(timer);
    removeEventListener('pointerdown', outside);
    removeEventListener('keydown', esc);
    document.removeEventListener('visibilitychange', visible);
  });
</script>

{#if !gone}
  <div class="wrap" bind:this={wrap}>
    <button class="btn ghost sm tb" class:on={open} title="Tasks" aria-expanded={open} onclick={toggle}>
      {#if running}<span class="spinner acc"></span>{:else if failed}<CircleX size={14} color="var(--red)" />{:else}<ListChecks size={14} />{/if}
      {#if running}<span>{running}</span>{/if}
    </button>
    {#if open}
      <div class="pop popover">
        <div class="ph"><b>Tasks</b><span class="sp"></span>
          {#if jobs.some((j) => j.done)}<button class="btn ghost sm" onclick={clearFinished}>Clear finished</button>{/if}
        </div>
        <div class="items">
          {#each [...jobs].reverse() as j (j.id)}
            <div class="job">
              <div class="row top">
                <span class="st">
                  {#if !j.done}<span class="spinner acc"></span>{:else if j.error}<CircleX size={15} color="var(--red)" />{:else}<CircleCheck size={15} color="var(--green)" />{/if}
                </span>
                <div class="txt">
                  <div class="t">{j.title}</div>
                  <div class="d" class:bad={j.error}>{j.error || j.detail}</div>
                </div>
                {#if j.done && j.error}<button class="btn ghost sm" onclick={() => copy(j)}><Copy size={12} />Copy details</button>{/if}
                <button class="btn ghost sm" title={j.done ? 'Remove from the list' : 'Stops at the next safe point'} onclick={() => act(j)}>{j.done ? 'Dismiss' : 'Cancel'}</button>
              </div>
              {#if !j.done}
                <div class="bar pb" class:busy={j.progress == null}><i style:width="{(j.progress ?? 0) * 100}%"></i></div>
              {/if}
              {#each j.notes || [] as n, i (i)}<div class="note">{n}</div>{/each}
            </div>
          {:else}
            <div class="none">Nothing running. Pack updates and installs show up here.</div>
          {/each}
        </div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .wrap { position: relative; }
  .tb { gap: 5px; }
  .acc { color: var(--acc); }
  .pop { position: absolute; right: 0; top: calc(100% + 6px); width: min(400px, calc(100vw - 32px)); z-index: 65; }
  .ph { display: flex; align-items: center; padding: 12px 16px 6px; }
  .ph b { font-size: 13px; font-weight: 600; }
  .items { max-height: min(26rem, calc(100vh - 120px)); overflow: auto; padding: 0 16px 12px; }
  .job { display: flex; flex-direction: column; gap: 4px; padding: 6px 0; }
  .top { gap: 12px; }
  .st { flex: none; display: grid; place-items: center; width: 16px; }
  .txt { flex: 1; min-width: 0; }
  .t { font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .d { font-size: 11px; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .d.bad { color: var(--red); }
  .pb { margin-left: 28px; }
  .note { margin-left: 28px; font-size: 11.5px; color: var(--gold); }
  .none { padding: 24px 0; text-align: center; color: var(--muted); font-size: 12px; }
</style>
