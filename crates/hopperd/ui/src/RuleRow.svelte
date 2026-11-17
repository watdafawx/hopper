<script>
  // One row of the Rules list: a kind chip, the rule as a "When X, then Y"
  // sentence (the trigger and the outcome in bold), the server chip, its last
  // run or last error, then the buttons that kind has and the on/off switch.
  // A warning's number is edited right in the sentence.
  import { Trash2, Send, Play, Pencil, AlertTriangle, CheckCircle2 } from '@lucide/svelte';
  import { KIND_LABEL } from './lib/rulesList.js';
  import { since } from './lib/rules.js';

  let { item, ix, on, pending = false, multi = false, open = false, picking = false, covered = [], ontoggle, onswitchdown, ontest, onrun, onrunon, onedit, onremove, onnum, onlog } = $props();

  /** Numbers a warning row edits in place: saved when you leave the box or press Enter. */
  function commit(e, p) {
    const v = Number(e.currentTarget.value);
    if (!Number.isFinite(v) || v < p.min || v > p.max) {
      e.currentTarget.value = String(p.value);
      onnum(p, null);
      return;
    }
    if (v !== p.value) onnum(p, v);
  }
</script>

<div class="rule row-hover" class:off={!on} data-pick-ix={ix} data-kind={item.kind}>
  <span class="k {item.kind}">{KIND_LABEL[item.kind]}</span>
  <div class="grow">
    <div class="sent">{#each item.parts as p, i (i)}{#if p.field}<input class="nin" type="number" min={p.min} max={p.max} step={p.step} value={p.value} aria-label="Number for this warning" onchange={(e) => commit(e, p)} onkeydown={(e) => e.key === 'Enter' && e.currentTarget.blur()} />{:else if p.b}<b>{p.text}</b>{:else}{p.text}{/if}{/each}</div>
    <div class="meta tiny muted">
      {#if item.kind !== 'warning' && item.name}<span class="nm">{item.name}</span>{/if}
      <span class="chip">{item.serverText}</span>
      {#if item.extra}<span class="ex">{item.extra}</span>{/if}
      {#if item.kind === 'message' && item.last}
        <span class="bad"><AlertTriangle size={11} /> Last message failed: {item.last.text}</span>
      {:else if item.kind === 'action'}
        {#if item.last}
          <button class="last" class:bad={!item.last.ok} onclick={onlog} title="Last runs">
            {#if item.last.ok}<CheckCircle2 size={11} />{:else}<AlertTriangle size={11} />{/if}
            {item.last.ok ? 'Ran' : 'Failed'} {since(item.last.at)}{multi ? ` on ${item.last.server}` : ''}{item.last.manual ? ' (by hand)' : ''}{item.last.ok ? '' : `: ${item.last.text}`}
          </button>
        {:else}
          <span class="subtle">Hasn't run yet</span>
        {/if}
      {/if}
    </div>
    {#if open && item.kind === 'action'}
      <div class="log">
        {#each item.src.history as h, i (i)}
          <div class:bad={!h.ok}>{new Date(h.at * 1000).toLocaleString()} · {h.server} · {h.text}{h.manual ? ' (by hand)' : ''}</div>
        {/each}
      </div>
    {/if}
    {#if picking}
      <div class="pick"><span class="tiny muted">Run on</span>
        {#each covered as s (s.id)}<button class="chip acc" onclick={() => onrunon(s)}>{s.name}</button>{/each}
      </div>
    {/if}
  </div>
  {#if item.kind === 'message'}<button class="btn sm icon ghost" title="Send a test" aria-label="Send a test" onclick={ontest}><Send size={13} /></button>{/if}
  {#if item.kind === 'action'}<button class="btn sm icon ghost" title="Run now" aria-label="Run now" onclick={onrun}><Play size={13} /></button>{/if}
  {#if item.kind !== 'warning'}
    <button class="btn sm icon ghost" title="Edit" aria-label="Edit" onclick={onedit}><Pencil size={13} /></button>
    <button class="btn sm icon ghost" title="Remove" aria-label="Remove" onclick={onremove}><Trash2 size={13} /></button>
  {/if}
  <label class="switch" class:pend={pending} title={item.enabled ? 'Turn off' : 'Turn on'} onpointerdown={onswitchdown}><input type="checkbox" checked={on} aria-label={item.enabled ? 'Turn off' : 'Turn on'} onclick={ontoggle} /><span></span></label>
</div>

<style>
  .rule { display: flex; align-items: flex-start; gap: 8px; padding: 9px 4px; border-bottom: 1px solid var(--line); border-radius: var(--radius); }
  .rule .btn, .rule .switch { margin-top: 0; }
  .rule .switch { align-self: flex-start; margin-top: 2px; }
  .rule.off .grow, .rule.off .k { opacity: 0.55; }
  .k { flex: none; width: 62px; margin-top: 2px; text-align: center; font-size: 11px; line-height: 18px; height: 18px; border-radius: var(--radius-sm); }
  .k.message { color: var(--ink-blue); background: var(--blue-dim); }
  .k.warning { color: var(--ink-gold); background: var(--gold-dim); }
  .k.action { color: var(--ink-violet); background: var(--violet-dim); }
  .grow { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 3px; }
  .sent { color: var(--subtle); overflow-wrap: anywhere; line-height: 22px; }
  .sent b { color: var(--text); font-weight: 600; }
  .nin { width: 58px; height: 22px; margin: 0 2px; padding: 0 4px; text-align: center; background: var(--base); border: 1px solid var(--input-line); border-radius: var(--radius-sm); color: var(--text); font: inherit; font-weight: 600; vertical-align: top; }
  .nin:focus { outline: none; border-color: var(--acc); }
  .meta { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 8px; }
  .nm { color: var(--subtle); overflow-wrap: anywhere; }
  .ex { overflow-wrap: anywhere; }
  .bad { color: var(--red-text); display: inline-flex; align-items: center; gap: 4px; overflow-wrap: anywhere; text-align: left; }
  .last { display: inline-flex; align-items: center; gap: 4px; color: var(--green); text-align: left; overflow-wrap: anywhere; font: inherit; }
  .last.bad { color: var(--red-text); }
  .log { font: 11px/1.6 var(--mono); color: var(--subtle); background: var(--term); border-radius: var(--radius); padding: 6px 8px; margin-top: 4px; max-height: 160px; overflow-y: auto; }
  .log .bad { display: block; }
  .pick { display: flex; flex-wrap: wrap; gap: 5px; align-items: center; margin-top: 4px; }
  .pick .chip { cursor: pointer; }
  .switch.pend span { box-shadow: 0 0 0 2px var(--bg), 0 0 0 4px var(--acc); }
</style>
