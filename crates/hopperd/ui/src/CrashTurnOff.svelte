<script>
  // "Turn off…" on the crash card: the mods it would turn off, each with
  // how sure and why, before anything changes (desktop's
  // crash_view::turn_off_picker). All ticked to start; untick to keep one.
  // Nothing is turned off until the confirm button.
  import { untrack } from 'svelte';
  import { PowerOff } from '@lucide/svelte';
  import { startPick, clickSwallowed, tickLabel } from './lib/dragpick.svelte.js';

  /** mod(file) and tag(suspect) are the card's snippets, so names and tags look the same. */
  let { suspects, mod, tag, named, nameOf, busy = false, onConfirm, onCancel } = $props();

  let picked = $state(new Set(untrack(() => suspects.map((s) => s.file_name))));
  const files = $derived(suspects.filter((s) => picked.has(s.file_name)).map((s) => s.file_name));

  function toggle(file, e) {
    if (clickSwallowed()) return e.preventDefault();
    const s = new Set(picked);
    s.has(file) ? s.delete(file) : s.add(file);
    picked = s;
  }

  // Drag across the tick boxes to tick (or untick) several.
  function tickDown(e, file, ix) {
    const on = !picked.has(file);
    startPick(e, { list: 'crash-off', kind: 'tick', ix, on, selected: picked, keyAt: (i) => suspects[i]?.file_name ?? null, onTick: (s) => (picked = s), label: tickLabel(on) });
  }
</script>

<div class="pick">
  <b>Turn off these mods?</b>
  <div class="tiny muted">Untick any to keep. Mods that need them stop loading too. Turn them back on in Mods.</div>
  <div class="list" data-pick-list="crash-off">
    {#each suspects as s, ix (s.file_name)}
      <div class="it" data-pick-ix={ix}>
        <input type="checkbox" aria-label="Turn off {nameOf(s.file_name)}" checked={picked.has(s.file_name)} onpointerdown={(e) => tickDown(e, s.file_name, ix)} onclick={(e) => toggle(s.file_name, e)} />
        <div class="txt">
          <div class="line">{@render mod(s.file_name)}{@render tag(s)}</div>
          {#if s.why}<div class="tiny muted">{named(s.why)}</div>{/if}
        </div>
      </div>
    {/each}
  </div>
  <div class="row acts">
    <button class="btn sm danger" disabled={busy || !files.length} onclick={() => onConfirm(files)}><PowerOff size={13} />{files.length === 1 ? 'Turn off 1 mod' : `Turn off ${files.length} mods`}</button>
    <button class="btn sm ghost" disabled={busy} onclick={onCancel}>Cancel</button>
  </div>
</div>

<style>
  .pick { display: flex; flex-direction: column; gap: 6px; padding: 10px 12px; border: 1px solid var(--line2); border-radius: var(--radius); background: var(--surface); }
  .pick > b { font-size: 13px; font-weight: 600; }
  .list { display: flex; flex-direction: column; }
  .it { display: flex; gap: 8px; align-items: flex-start; padding: 4px 0; }
  .it input { margin-top: 4px; flex: none; }
  .txt { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
  .txt .tiny { white-space: normal; overflow-wrap: anywhere; }
  .line { font-size: 13px; line-height: 22px; }
  .line > :global(*) { margin-right: 5px; vertical-align: middle; }
  .acts { gap: 6px; flex-wrap: wrap; align-items: center; padding-top: 2px; }
</style>
