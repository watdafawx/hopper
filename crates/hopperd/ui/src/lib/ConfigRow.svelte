<script>
  // One setting, as desktop's config view shows it: its friendly name with
  // the raw key under it in mono, the description and limits, a dot when it
  // differs from the default the file states, Reset when it says one, a
  // timing chip when the file says it needs a restart, and the control its
  // kind gets (switch, slider + field, field with − / +, choice, list,
  // colour, text, or "Edit as text" for what can't be a setting).
  import { RotateCcw, X, Plus, Minus, FileText } from '@lucide/svelte';
  import { valueOf, edited, errorOf, setEdit, same, limits, check, listText, listItems, swatchOf, colorText } from './configs.svelte.js';

  let { rel, e, stacked = false, onEditText } = $props();

  const value = $derived(valueOf(rel, e));
  const dirty = $derived(edited(rel, e));
  const saveError = $derived(errorOf(rel, e));
  const lim = $derived(limits(e));
  const changed = $derived(e.default_value != null ? !same(value, e.default_value) : e.is_default === false && !dirty);
  const canReset = $derived(e.default_value != null && !same(value, e.default_value));
  const key = $derived([...(e.path || []), e.key].join('.'));

  /** What's typed in a field before it's taken, and why it isn't. */
  let draft = $state(null);
  let localError = $state('');
  let adding = $state('');
  const error = $derived(localError || saveError);
  // The value moved from outside (Reset, Discard, a review that dropped the edit, a save): a typed mistake no longer applies.
  $effect(() => {
    value;
    localError = '';
  });

  function take(text) {
    const err = check(e, text);
    localError = err;
    if (err) return;
    draft = null;
    setEdit(rel, e, text);
  }

  function fieldKeys(ev) {
    if (ev.key === 'Enter') {
      ev.preventDefault();
      take(ev.currentTarget.value);
    } else if (ev.key === 'Escape') {
      draft = null;
      localError = '';
      ev.currentTarget.value = value;
    }
  }

  const step = $derived(e.kind === 'int' ? 1 : e.slider && e.slider[1] - e.slider[0] <= 2 ? 0.01 : 0.1);
  function bump(dir) {
    const n = Number(draft ?? value) || 0;
    let next = e.kind === 'int' ? Math.round(n + dir) : Math.round((n + dir * step) * 1000) / 1000;
    if (e.range?.[0] != null) next = Math.max(e.range[0], next);
    if (e.range?.[1] != null) next = Math.min(e.range[1], next);
    take(e.kind === 'float' && Number.isInteger(next) ? `${next}.0` : String(next));
  }
  function slide(v) {
    const n = Number(v);
    take(e.kind === 'float' ? (Number.isInteger(n) ? `${n}.0` : String(n)) : String(Math.round(n)));
  }

  const items = $derived(e.kind === 'list' ? (dirty ? listItems(value) : e.list || []) : []);
  function setItems(next) {
    setEdit(rel, e, listText(next, e));
  }
  function addItem() {
    const t = adding.trim();
    if (!t) return;
    setItems([...items, t]);
    adding = '';
  }
  /** Drag to reorder a list's items. */
  let dragFrom = $state(-1);
  function dropAt(i) {
    if (dragFrom < 0 || dragFrom === i) return;
    const next = [...items];
    const [x] = next.splice(dragFrom, 1);
    next.splice(i, 0, x);
    dragFrom = -1;
    setItems(next);
  }

  const swatch = $derived(e.kind === 'color' ? swatchOf(e, value) : null);
  const restartText = $derived(e.restart && e.restart !== 'none' ? 'Next restart' : '');
</script>

<div class="cr" class:stacked class:dirty class:bad={!!error} data-line={e.line}>
  <div class="txt">
    <div class="k">
      {#if changed || dirty}<span class="cdot" class:unsaved={dirty} title={dirty ? 'Not saved yet' : 'Differs from the default'}></span>{/if}
      <span class="nm">{e.label || e.key}</span>
      {#if restartText}<span class="chip warn tc" title={e.restart === 'game' ? 'The file says this needs a restart' : 'The file says this needs the world reloaded: restart the server'}>{restartText}</span>{/if}
      {#if e.guessed}<span class="chip tc" title="This kind of file doesn't say what a setting takes: Hopper went by the value.">guessed</span>{/if}
    </div>
    <div class="raw mono" title={key}>{key}</div>
    {#if e.comment}<div class="d">{e.comment}</div>{/if}
    {#if lim}<div class="cap">{lim}</div>{/if}
    {#if error}<div class="cap err">{error}{localError ? ' Not kept.' : ''}</div>{/if}
  </div>
  <div class="ctl">
    {#if e.kind === 'bool'}
      <label class="switch big" title={value === 'true' ? 'On' : 'Off'}>
        <input type="checkbox" checked={value === 'true'} onchange={(ev) => setEdit(rel, e, ev.currentTarget.checked ? 'true' : 'false')} aria-label={e.label} />
        <span></span>
      </label>
    {:else if (e.kind === 'int' || e.kind === 'float') && e.slider}
      <div class="sl">
        <input type="range" min={e.slider[0]} max={e.slider[1]} step={step} value={Number(draft ?? value)} oninput={(ev) => (draft = ev.currentTarget.value)} onchange={(ev) => slide(ev.currentTarget.value)} aria-label={e.label} />
        <input class="input num mono" value={draft ?? value} onkeydown={fieldKeys} oninput={() => (localError = '')} onchange={(ev) => take(ev.currentTarget.value)} aria-label="{e.label} value" />
      </div>
    {:else if e.kind === 'int' || e.kind === 'float'}
      <div class="sl">
        <input class="input num wide mono" value={draft ?? value} onkeydown={fieldKeys} oninput={() => (localError = '')} onchange={(ev) => take(ev.currentTarget.value)} aria-label={e.label} />
        <button class="btn icon ghost step" title="Less" aria-label="Less" onclick={() => bump(-1)}><Minus size={13} /></button>
        <button class="btn icon ghost step" title="More" aria-label="More" onclick={() => bump(1)}><Plus size={13} /></button>
      </div>
    {:else if e.kind === 'choice'}
      <select class="input pick" value={value} onchange={(ev) => setEdit(rel, e, ev.currentTarget.value)} aria-label={e.label}>
        {#each e.choices as c (c)}<option value={c}>{c}{e.default_value === c ? ' · default' : ''}</option>{/each}
        {#if !e.choices.includes(value)}<option value={value}>{value}</option>{/if}
      </select>
    {:else if e.kind === 'list'}
      <div class="list">
        {#each items as it, i (i)}
          <div class="li" role="listitem" draggable="true" ondragstart={() => (dragFrom = i)} ondragover={(ev) => ev.preventDefault()} ondrop={() => dropAt(i)} title="Drag to reorder">
            <span class="grip" aria-hidden="true">⋮⋮</span><span class="mono ell">{it}</span>
            <button class="btn icon ghost rm" title="Remove {it}" aria-label="Remove {it}" onclick={() => setItems(items.filter((_, j) => j !== i))}><X size={12} /></button>
          </div>
        {:else}
          <div class="cap">Empty list</div>
        {/each}
        <div class="add">
          <input class="input mono grow" placeholder="Add an item…" bind:value={adding} onkeydown={(ev) => ev.key === 'Enter' && (ev.preventDefault(), addItem())} />
          <button class="btn" disabled={!adding.trim()} onclick={addItem}>Add</button>
        </div>
      </div>
    {:else if e.kind === 'color'}
      <div class="sl">
        <label class="sw" style:background={swatch || 'transparent'} title="Pick a colour">
          <input type="color" value={swatch || '#000000'} onchange={(ev) => setEdit(rel, e, colorText(e, ev.currentTarget.value))} aria-label={e.label} />
        </label>
        <input class="input mono hex" value={draft ?? value} oninput={() => (localError = '')} onkeydown={fieldKeys} onchange={(ev) => take(ev.currentTarget.value)} aria-label="{e.label} value" />
      </div>
    {:else if e.kind === 'opaque'}
      <button class="btn" onclick={() => onEditText?.(rel, e.line)}><FileText size={13} />Edit as text · line {e.line}</button>
    {:else}
      <input class="input txtv" class:mono={!e.masked} value={draft ?? value} placeholder={e.masked ? 'Hidden. Type to replace it.' : ''} onfocus={(ev) => e.masked && value === e.value && (ev.currentTarget.value = '')} onblur={(ev) => e.masked && ev.currentTarget.value === '' && (ev.currentTarget.value = value)} onkeydown={fieldKeys} onchange={(ev) => (!e.masked || ev.currentTarget.value !== '') && take(ev.currentTarget.value)} aria-label={e.label} />
    {/if}
    {#if e.default_value != null}
      <button class="btn icon ghost reset" class:hid={!canReset} disabled={!canReset} title="Back to default {e.default_value}" aria-label="Back to default" onclick={() => { draft = null; localError = ''; setEdit(rel, e, e.default_value); }}><RotateCcw size={13} /></button>
    {/if}
  </div>
</div>

<style>
  .cr { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 16px; align-items: start; padding: 9px 12px; border-radius: var(--radius); }
  .cr:hover { background: var(--hover); }
  .cr.dirty { background: #f973160d; }
  .cr.bad { box-shadow: inset 3px 0 var(--red); }
  .txt { min-width: 0; display: flex; flex-direction: column; gap: 1px; }
  .k { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; font-weight: 600; font-size: 13px; }
  .cdot { width: 6px; height: 6px; border-radius: 50%; background: var(--acc); flex: none; }
  .cdot.unsaved { box-shadow: 0 0 0 2px #f9731644; }
  .tc { height: 18px; font-size: 11px; font-weight: 500; padding: 0 6px 1px; }
  .raw { font-size: 11px; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .d { color: var(--subtle); font-size: 12px; white-space: pre-line; }
  .cap { color: var(--muted); font-size: 11.5px; }
  .cap.err { color: var(--red); }
  .ctl { display: flex; align-items: flex-start; gap: 6px; justify-content: flex-end; min-width: 0; }
  .sl { display: flex; align-items: center; gap: 8px; }
  .sl input[type='range'] { width: 150px; accent-color: var(--acc); }
  .input { padding: 4px 8px; color: var(--text); }
  .num { width: 76px; text-align: right; outline: none; }
  .num.wide { width: 120px; }
  .hex { width: 112px; outline: none; }
  .txtv { width: 260px; outline: none; }
  .pick { width: 220px; outline: none; }
  .switch.big { margin-top: 2px; }
  .step { padding: 4px 6px; }
  .sw { width: 28px; height: 28px; border-radius: var(--radius); border: 1px solid var(--line2); position: relative; cursor: pointer; flex: none; overflow: hidden; }
  .sw input { position: absolute; inset: 0; opacity: 0; cursor: pointer; width: 100%; height: 100%; }
  .list { width: 300px; display: flex; flex-direction: column; gap: 3px; }
  .li { display: flex; align-items: center; gap: 8px; padding: 2px 4px 2px 8px; border-radius: var(--radius-sm); background: var(--surface); font-size: 12px; cursor: grab; }
  .grip { color: var(--muted); font-size: 10px; letter-spacing: -2px; }
  .ell { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .rm { padding: 2px 4px; }
  .add { display: flex; gap: 6px; margin-top: 2px; }
  .grow { flex: 1; min-width: 0; outline: none; }
  .reset { padding: 4px 6px; }
  .reset.hid { visibility: hidden; }
  /* The side panel and phones: the control under its label, full width. */
  .cr.stacked { grid-template-columns: minmax(0, 1fr); gap: 6px; }
  .stacked .ctl { justify-content: flex-start; flex-wrap: wrap; }
  .stacked .txtv, .stacked .pick, .stacked .list { width: 100%; flex: 1; }
  .stacked .sl { flex: 1; }
  .stacked .sl input[type='range'] { flex: 1; width: auto; }
  @media (max-width: 700px) {
    .cr { grid-template-columns: minmax(0, 1fr); gap: 6px; }
    .ctl { justify-content: flex-start; flex-wrap: wrap; }
    .txtv, .pick, .list { width: 100%; flex: 1; }
    .sl { flex: 1; }
    .sl input[type='range'] { flex: 1; width: auto; }
    /* 44px touch targets. */
    .input, .btn, .sw { min-height: 44px; }
    .btn.icon { min-width: 44px; justify-content: center; }
    .switch.big { transform: scale(1.35); transform-origin: left center; margin: 12px 8px 12px 0; }
  }
</style>
