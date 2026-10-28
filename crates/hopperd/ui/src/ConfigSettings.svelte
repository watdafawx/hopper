<script>
  // A TOML config as settings, like desktop's config editor: each table's
  // title and comment, each setting with its comment, limits and default, a
  // switch for true/false, a picker for allowed values, and other values
  // typed into a field (Enter takes it, Escape drops it). The setting the
  // editor opened at is marked and scrolled to.
  import { tick } from 'svelte';
  import { Search } from '@lucide/svelte';
  import { say } from './lib/toast.svelte.js';

  let { sections, query = '', focusLine = 0, onSet } = $props();

  /** The setting being typed into: `${table}\u0000${key}`, its text, its error. */
  let editing = $state('');
  let typed = $state('');
  let fieldError = $state('');
  let list = $state();

  const idOf = (s, e) => `${s.path.join('.')}\u0000${e.key}`;
  const fmt = (x) => String(x);

  function limits(e) {
    const out = [];
    if (e.range) {
      const [lo, hi] = e.range;
      if (lo != null && hi != null) out.push(`${fmt(lo)} to ${fmt(hi)}`);
      else if (lo != null) out.push(`at least ${fmt(lo)}`);
      else if (hi != null) out.push(`at most ${fmt(hi)}`);
    }
    if (e.default != null) out.push(`default ${e.default}`);
    return out.join(' · ');
  }

  const rows = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const out = [];
    for (const s of sections) {
      const inPath = q && s.path.join('.').toLowerCase().includes(q);
      const hits = s.entries.filter((e) => !q || inPath || e.key.toLowerCase().includes(q) || e.comment.toLowerCase().includes(q));
      if (!hits.length) continue;
      out.push({ s, hits });
    }
    return out;
  });

  /** Sets one setting; resolves to the field's error, or '' when taken. */
  const set = (s, e, value) => onSet(s.path, e.key, value);
  /** A switch or picker: a refused value says why. */
  async function pick(ev, s, e, value) {
    const box = ev.currentTarget;
    const err = await set(s, e, value);
    if (!err) return;
    say(`${e.key}: ${err}`, true);
    // Back to what the file says.
    if (box.type === 'checkbox') box.checked = e.value === 'true';
    else box.value = e.value;
  }

  async function startEdit(s, e) {
    await commit();
    editing = idOf(s, e);
    typed = e.value;
    fieldError = '';
    await tick();
    list?.querySelector('input.field')?.focus();
  }

  /** The commit running (Enter, then a blur or a click on another value
   * while it's on its way, would otherwise send it twice and could close
   * the next field when it lands). */
  let committing = null;
  /** Takes what was typed into the open field, if it's a valid value. */
  function commit() {
    if (!committing) committing = commitNow().finally(() => (committing = null));
    return committing;
  }
  async function commitNow() {
    if (!editing) return true;
    const was = editing;
    const [table, key] = editing.split('\u0000');
    const s = sections.find((x) => x.path.join('.') === table);
    const e = s?.entries.find((x) => x.key === key);
    if (!e) {
      editing = '';
      return true;
    }
    if (typed === e.value) {
      editing = '';
      return true;
    }
    const err = await set(s, e, typed);
    // Escape pressed meanwhile: the field is gone, the value still taken.
    if (editing !== was) return !err;
    if (err) {
      fieldError = err;
      return false;
    }
    editing = '';
    fieldError = '';
    return true;
  }

  /** For the editor: takes an open field before switching view or saving. */
  export async function flush() {
    return commit();
  }
  export function cancelEdit() {
    if (!editing) return false;
    editing = '';
    fieldError = '';
    return true;
  }

  function fieldKeys(ev) {
    if (ev.key === 'Enter') {
      ev.preventDefault();
      commit();
    } else if (ev.key === 'Escape') {
      ev.preventDefault();
      ev.stopPropagation();
      cancelEdit();
    }
  }

  // Scroll to the setting the editor opened at, with its table's title.
  let scrolledTo = 0;
  $effect(() => {
    const line = focusLine;
    rows;
    if (!line || line === scrolledTo || !list) return;
    tick().then(() => {
      const el = list?.querySelector(`[data-line="${line}"]`);
      if (!el) return;
      scrolledTo = line;
      const head = el.closest('.sec')?.querySelector('.sh');
      (head && head.offsetTop > el.offsetTop - 200 ? head : el).scrollIntoView({ block: 'start' });
    });
  });
</script>

<div class="cs" bind:this={list}>
  {#each rows as { s, hits } (s.path.join('.'))}
    <div class="sec">
      {#if s.path.length}
        <div class="sh">
          <div class="st">{s.path.join(' › ')}</div>
          {#if s.comment}<div class="tiny muted wrap">{s.comment}</div>{/if}
        </div>
      {/if}
      {#each hits as e (e.key)}
        {@const id = idOf(s, e)}
        {@const lim = limits(e)}
        <div class="ent row-hover" class:row-sel={focusLine === e.line} data-line={e.line}>
          <div class="txt">
            <div class="k">{e.key}</div>
            {#if e.comment}<div class="tiny muted wrap">{e.comment}</div>{/if}
            {#if lim}<div class="tiny cap">{lim}</div>{/if}
          </div>
          <div class="ctl">
            {#if e.kind === 'bool'}
              <label class="switch" title={e.value === 'true' ? 'On' : 'Off'}>
                <input type="checkbox" checked={e.value === 'true'} onchange={(ev) => pick(ev, s, e, ev.currentTarget.checked ? 'true' : 'false')} />
                <span></span>
              </label>
            {:else if e.kind === 'str' && e.allowed.length && editing !== id}
              <select class="input pick" value={e.value} onchange={(ev) => pick(ev, s, e, ev.currentTarget.value)}>
                {#each e.allowed as a (a)}<option value={a}>{a}</option>{/each}
                {#if !e.allowed.includes(e.value)}<option value={e.value}>{e.value}</option>{/if}
              </select>
            {:else if editing === id}
              <div class="fw">
                <input class="input field mono" bind:value={typed} onkeydown={fieldKeys} oninput={() => (fieldError = '')} onblur={() => !fieldError && commit()} />
                {#if fieldError}<div class="tiny bad">{fieldError}</div>{/if}
              </div>
            {:else}
              <button class="btn sm val mono" title="Click to change" onclick={() => startEdit(s, e)}><span class="ell">{e.value === '' ? '(empty)' : e.value}</span></button>
            {/if}
          </div>
        </div>
      {/each}
    </div>
  {:else}
    <div class="empty-state">
      <span class="es-ic"><Search size={18} /></span>
      <b>{query.trim() ? 'No settings match' : 'No settings'}</b>
      <p>{query.trim() ? 'Try another word, or look in the Text view.' : 'This file has no settings to show here. The Text view has all of it.'}</p>
    </div>
  {/each}
</div>

<style>
  .cs { flex: 1; min-height: 300px; overflow: auto; padding: 4px 6px 12px; background: var(--base); }
  .sh { padding: 12px 12px 4px; }
  .st { font-size: 11.5px; font-weight: 600; color: var(--muted); }
  .wrap { white-space: normal; }
  .ent { display: flex; align-items: flex-start; gap: 16px; padding: 8px 12px; border-radius: 6px; }
  .txt { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
  .k { font-size: 13px; font-weight: 600; }
  .cap { color: var(--subtle); }
  .ctl { width: 256px; flex: none; display: flex; justify-content: flex-end; }
  .val { width: 100%; justify-content: flex-start; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .pick { width: 100%; padding: 4px 8px; }
  .fw { width: 100%; display: flex; flex-direction: column; gap: 3px; }
  .field { width: 100%; padding: 4px 8px; }
  .bad { color: var(--red); }
  .switch { margin-top: 2px; }
</style>
