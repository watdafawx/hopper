<script>
  // Settings › Sleep: stop the server when nobody is on, and start it when
  // someone joins through its address (the router answers meanwhile).
  import { untrack } from 'svelte';
  import { Moon, Save, TriangleAlert } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { since } from './lib/rules.js';

  let { server } = $props();
  // The live feed hands over a new `server` object every second.
  const sid = $derived(server.id);
  let info = $state(null);
  let sleepOn = $state(false);
  let minutes = $state(15);
  let wake = $state(true);
  let who = $state('anyone');
  let retry = $state(0);
  let message = $state('');
  let starting = $state('lobby');
  let modded = $state('kick');
  let saved = $state('');
  let saving = $state(false);

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const body = $derived({
    sleep_after_minutes: sleepOn ? Math.max(1, Number(minutes) || 1) : 0,
    wake_on_join: wake,
    wake_who: who,
    retry_after_minutes: Number(retry) || 0,
    sleep_message: message.trim(),
    while_starting: starting,
    modded_players: modded,
  });
  const dirty = $derived(info && JSON.stringify(body) !== saved);
  const shown = $derived(message.trim() || info?.default_sleep_message || '');
  const WHO = [
    ['anyone', 'Anyone'],
    ['whitelist', 'Whitelist'],
    ['ops', 'Ops'],
  ];
  const STARTING = [
    ['lobby', 'Lobby island'],
    ['kick', 'Kick with rejoin message'],
  ];
  const MODDED = [
    ['kick', 'Join again message'],
    ['wait', 'Wait (experimental)'],
  ];
  const RETRY = [
    [0, 'Wait for a start here'],
    [2, '2 min'],
    [10, '10 min'],
    [30, '30 min'],
  ];

  function stateOf(i) {
    return { sleep_after_minutes: i.sleep_after_minutes, wake_on_join: i.wake_on_join, wake_who: i.wake_who, retry_after_minutes: i.retry_after_minutes, sleep_message: i.sleep_message || '', while_starting: i.while_starting, modded_players: i.modded_players };
  }

  async function load() {
    try {
      info = await api(`${base}/address`);
      sleepOn = info.sleep_after_minutes > 0;
      minutes = info.sleep_after_minutes || 15;
      wake = info.wake_on_join;
      who = info.wake_who;
      retry = info.retry_after_minutes;
      message = info.sleep_message || '';
      starting = info.while_starting || 'lobby';
      modded = info.modded_players || 'kick';
      saved = JSON.stringify(stateOf(info));
    } catch (e) {
      say(e.message, true);
    }
  }

  async function save() {
    saving = true;
    try {
      await api(`${base}/address`, { method: 'PUT', body });
      say(!body.sleep_after_minutes ? 'Sleep turned off' : body.wake_on_join ? `${server.name} sleeps after ${body.sleep_after_minutes} min and wakes when someone joins` : `${server.name} sleeps after ${body.sleep_after_minutes} min`);
      await load();
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  const when = since;
  // A failed wake holds joins back until a start here, or until the retry
  // time (as saved) has passed.
  const failed = $derived.by(() => {
    const f = info?.wake_failed;
    if (!f || !sleepOn || !wake) return null;
    const r = info.retry_after_minutes;
    return r && Date.now() / 1000 >= f.at + r * 60 ? null : f;
  });

  $effect(() => {
    sid;
    load();
  });
  // A wake or a start changes what the card says (failure, last woken).
  // Only the state is watched: reading `info` here would re-run this each time
  // load() sets it, an endless loop of requests that also wiped unsaved edits.
  // `server` itself is a new object every second, so watch a derived value.
  const powerState = $derived(server.state);
  $effect(() => {
    powerState;
    untrack(() => {
      if (info && !dirty) load();
    });
  });
</script>

<div class="card side col">
  <h3><Moon size={14} />Sleep<span class="sp"></span>
    {#if dirty}<span class="unsaved">Not saved</span>{/if}
    <button class="btn sm pri" disabled={!dirty || saving} onclick={save}><Save size={13} />Save</button>
  </h3>
  {#if info}
    {#if failed}
      <div class="bad small"><TriangleAlert size={13} /><span>{failed.by} woke it {when(failed.at)}, but it {failed.why}. {retry ? `Joining tries again ${retry} min after that.` : "Joining won't start it again until you start it here."}</span></div>
    {/if}
    <label class="row small"><input type="checkbox" bind:checked={sleepOn} />Sleep when empty for
      <span class="input mins"><input type="number" min="1" max="1440" bind:value={minutes} disabled={!sleepOn} /></span>min</label>
    <!-- Always drawn so people can see wake-on-join exists; greyed and disabled
         while sleep is off. Values are kept and saved as they are. -->
    <div class="wakes col" class:off={!sleepOn} aria-disabled={!sleepOn}>
      <label class="row small"><input type="checkbox" bind:checked={wake} disabled={!sleepOn} />Wake when someone joins</label>
      {#if !sleepOn}
        <div class="tiny muted note">Turn on sleep above to use these: a server that sleeps wakes when someone joins.</div>
      {/if}
      {#if wake}
        <!-- Waking goes through the address router: say so when there's no address to join. -->
        {#if sleepOn}<div class="tiny muted note">{info.addresses?.length ? "Joining from Minecraft's server list starts it again. The first join takes about a minute." : 'Players wake it by joining its address, and it has none yet. Add one in Network & address.'}</div>{/if}
        <div class="f small"><span class="k">Who can wake it</span>
          <div class="seg" role="group" aria-label="Who can wake it">
            {#each WHO as [v, label] (v)}<button class:on={who === v} aria-pressed={who === v} disabled={!sleepOn} onclick={() => (who = v)}>{label}</button>{/each}
          </div>
        </div>
        <div class="tiny muted hint">
          {#if who === 'anyone'}Anyone who joins starts it.{:else if who === 'whitelist'}Players on the whitelist or the ops list.{:else}Only players on the ops list.{/if}
          Names aren't checked until the server is up, so this keeps strangers from waking it by accident, not on purpose.
        </div>
        <div class="f small"><span class="k">While it starts</span>
          <div class="seg" role="group" aria-label="While it starts">
            {#each STARTING as [v, label] (v)}<button class:on={starting === v} aria-pressed={starting === v} disabled={!sleepOn} onclick={() => (starting = v)}>{label}</button>{/each}
          </div>
        </div>
        <div class="tiny muted hint">
          {#if starting === 'lobby'}Players wait on a small island with the start's progress, then move over by themselves. Works for Minecraft 1.21 and 1.21.1; other versions and modded clients get the rejoin message. Hopper turns on accepts-transfers in server.properties for this.
          {:else}Players are told when to join again.{/if}
        </div>
        <div class="f small"><span class="k">Modded players</span>
          <div class="seg" role="group" aria-label="While it starts, modded players">
            {#each MODDED as [v, label] (v)}<button class:on={modded === v} aria-pressed={modded === v} disabled={!sleepOn} onclick={() => (modded = v)}>{label}</button>{/each}
          </div>
        </div>
        <div class="tiny muted hint">
          {#if modded === 'wait'}Modded players can wait on the connecting screen until the server is up. They see no progress bar.
          {:else}Modded players get a message to join again.{/if}
        </div>
        <div class="f small"><span class="k">After a failed start</span>
          <div class="seg" role="group" aria-label="After a failed start">
            {#each RETRY as [v, label] (v)}<button class:on={Number(retry) === v} aria-pressed={Number(retry) === v} disabled={!sleepOn} onclick={() => (retry = v)}>{label}</button>{/each}
          </div>
        </div>
        <div class="f small"><label class="k" for="sleep-msg">Server list message</label>
          <div class="input grow"><input id="sleep-msg" maxlength="80" bind:value={message} placeholder={info.default_sleep_message} disabled={!sleepOn} /></div>
        </div>
        <!-- How players see it in their server list while it sleeps. Stays visible
             (dimmed) with sleep off: it shows what turning sleep on gives them. -->
        <div class="mc" aria-label="Server list preview">
          <div class="mc-top"><span>{server.name}</span><span class="sp"></span><span class="mc-g">0/{server.max_players || 20}</span></div>
          <div class="mc-w">{server.name}</div>
          <div><span class="mc-y">{shown}</span><span class="mc-g">{` (${info.wake_estimate})`}</span></div>
        </div>
        {#if info.last_wake}<div class="tiny muted">Last woken by {info.last_wake.by}, {when(info.last_wake.at)}. The console notes each wake.</div>{/if}
      {:else if sleepOn}
        <div class="tiny muted note">Joining doesn't start it: players see "Offline" until someone starts it here.</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .side { gap: 8px; }
  .side h3 { margin-bottom: 2px; }
  .mins { width: 64px; padding: 2px 6px; display: inline-flex; }
  .unsaved { color: var(--gold); font-size: 11.5px; font-weight: 600; }
  .wakes { gap: 8px; }
  .wakes.off .seg button, .wakes.off .seg button:hover { cursor: default; background: none; color: var(--muted); }
  .wakes.off .seg button.on { background: var(--hover); color: var(--muted); }
  .wakes.off .mc { opacity: .6; }
  .wakes.off :is(label.row, .k) { color: var(--muted); }
  .wakes.off input:disabled { opacity: .6; cursor: default; }
  .f { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .k { width: 136px; flex: none; }
  .hint { margin-left: 144px; }
  .grow { flex: 1; min-width: 0; padding: 4px 8px; }
  .seg { display: inline-flex; border: 1px solid var(--line2); border-radius: var(--radius); overflow: hidden; }
  .seg button { padding: 3px 10px; font-size: 12px; color: var(--subtle); }
  .seg button + button { border-left: 1px solid var(--line2); }
  .seg button.on { background: var(--acc-dim); color: var(--acc); }
  .bad { display: flex; gap: 8px; align-items: flex-start; color: var(--red); background: var(--red-dim); border: 1px solid #ef444455; border-radius: var(--radius-lg); padding: 8px 10px; }
  .bad span { color: var(--text); }
  .mc { background: #1d1813; border: 1px solid #ffffff1f; border-radius: var(--radius); padding: 8px 10px; font-family: var(--mono); font-size: 12px; line-height: 1.5; color: #fff; }
  .mc-top { display: flex; }
  .mc-w { color: #fff; }
  .mc-y { color: #ffff55; }
  .mc-g { color: #aaa; }
  label.row input[type='checkbox'] { width: 15px; height: 15px; accent-color: var(--acc); }
  @media (max-width: 560px) {
    .f { flex-wrap: wrap; }
    .hint { margin-left: 0; }
  }
</style>
