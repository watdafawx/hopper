<script>
  // A mod's config files as settings: one chip per file with who reads it,
  // then the picked file's settings by section. The Configs page shows it
  // beside the mod list; the Mods tab shows it in a side panel (stacked).
  import { FileText, SlidersHorizontal, AlertTriangle } from '@lucide/svelte';
  import ConfigRow from './ConfigRow.svelte';
  import { cfg, loadFiles, edited, eid, same, sideChip, shortRel, fileRow, playersOf, playersDiffer } from './configs.svelte.js';
  import { say } from './toast.svelte.js';
  import { untrack } from 'svelte';

  let { server, rels = [], filter = 'all', side = 'all', query = '', stacked = false, onEditText, pick = $bindable('') } = $props();

  const shownRels = $derived(rels.filter((r) => side === 'all' || (cfg.files[r]?.side || fileRow(r)?.side) === side));
  let loading = $state(false);

  $effect(() => {
    const want = rels;
    const sid = server.id;
    if (!want.length) return;
    untrack(() => {
      loading = true;
      loadFiles(sid, want)
        .catch((e) => say(e.message, true))
        .finally(() => (loading = false));
    });
  });

  // The file shown: the one picked, else the first with a change, else the first.
  const current = $derived.by(() => {
    if (pick && shownRels.includes(pick)) return pick;
    return shownRels.find((r) => (filter === 'players' ? playersDiffer(r) : fileRow(r)?.changed || cfg.edits[r])) || shownRels[0] || '';
  });
  const file = $derived(cfg.files[current]);

  const words = $derived(query.trim().toLowerCase().split(/\s+/).filter(Boolean));
  function keep(e) {
    if (filter === 'unsaved' && !edited(current, e)) return false;
    if (filter === 'players' && !playersOf(current, e)) return false;
    if (filter === 'changed' && !(edited(current, e) || (e.default_value != null ? !same(e.value, e.default_value) : e.is_default === false))) return false;
    if (!words.length) return true;
    const hay = `${e.label} ${e.key} ${(e.path || []).join(' ')} ${e.comment}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  }
  const sections = $derived((file?.sections || []).map((s) => ({ s, rows: s.entries.filter(keep) })).filter((x) => x.rows.length));
  const unsavedIn = (r) => Object.keys(cfg.edits[r] || {}).length;
  const label = (path) => path.map((p) => p.replace(/[_-]+/g, ' ').replace(/([a-z])([A-Z])/g, '$1 $2').replace(/^./, (c) => c.toUpperCase())).join(' › ');
</script>

<div class="cv" class:stacked>
  {#if shownRels.length > 1 || stacked}
    <div class="files" role="tablist" aria-label="Config files">
      {#each shownRels as r (r)}
        {@const sc = sideChip(cfg.files[r]?.side || fileRow(r)?.side)}
        {@const n = unsavedIn(r)}
        <button class="fchip" class:on={r === current} role="tab" aria-selected={r === current} title={r} onclick={() => (pick = r)}>
          <span class="mono ell">{shortRel(r)}</span>
          {#if sc}<span class="chip sc {sc[1]}">{sc[0]}</span>{/if}
          {#if fileRow(r)?.live}{@const lv = fileRow(r).live}<span class="chip sc" class:running={lv.used} class:warn={!lv.used} title="Which copy the server reads">{lv.label}</span>{/if}
          {#if playersDiffer(r)}<span class="chip lan sc" title="Players' copy differs in {playersDiffer(r)} settings">≠ {playersDiffer(r)}</span>{/if}
          {#if n}<span class="chip acc sc">{n}</span>{/if}
        </button>
      {/each}
    </div>
  {/if}
  {#if current && file && !file.ok}
    <div class="empty-state">
      <span class="es-ic"><AlertTriangle size={18} /></span>
      <b>Can't show {shortRel(current)} as settings</b>
      <p>{file.error}</p>
      <button class="btn sm" onclick={() => onEditText?.(current, 0)}><FileText size={13} />Edit as text</button>
    </div>
  {:else if current && file}
    {#if !stacked}
      <div class="fhead">
        <span class="mono small subtle ell" title={current}>{current}</span>
        {#if fileRow(current)?.live}<span class="chip sc" class:running={fileRow(current).live.used} class:warn={!fileRow(current).live.used}>{fileRow(current).live.label}</span>{/if}
        <span class="sp"></span>
        <button class="btn sm ghost" onclick={() => onEditText?.(current, 0)}><FileText size={13} />Text view</button>
      </div>
    {/if}
    <div class="rows">
      {#each sections as { s, rows } (s.path.join('.'))}
        {#if s.path.length}
          <div class="sec">{label(s.path)}{#if s.comment}<span class="cap"> · {s.comment.split('\n')[0]}</span>{/if}</div>
        {/if}
        {#each rows as e (eid(e))}
          <ConfigRow rel={current} {e} {stacked} {onEditText} />
        {/each}
      {:else}
        <div class="empty-state">
          <span class="es-ic"><SlidersHorizontal size={18} /></span>
          <b>{filter === 'unsaved' ? 'No unsaved changes here' : filter === 'players' ? "Players' copy matches here" : filter === 'changed' ? 'Nothing changed here' : words.length ? 'No settings match' : 'No settings'}</b>
          <p>{filter !== 'all' ? 'Every setting in this file is as the file says by default, or it states no default.' : words.length ? 'Try another word.' : 'This file has nothing to show as settings. The Text view has all of it.'}</p>
        </div>
      {/each}
    </div>
    {#if stacked}
      <button class="btn sm ghost tv" onclick={() => onEditText?.(current, 0)}><FileText size={13} />Open {shortRel(current)} as text</button>
    {/if}
  {:else if loading || (current && !file)}
    <div class="skels">{#each [0, 1, 2, 3, 4] as i (i)}<div class="skel"><i></i></div>{/each}</div>
  {:else}
    <div class="empty-state">
      <span class="es-ic"><SlidersHorizontal size={18} /></span>
      <b>No config files{side !== 'all' ? ' for this side' : ''}</b>
      <p>{side !== 'all' ? 'Pick another side, or All.' : 'This mod has no config files Hopper can read.'}</p>
    </div>
  {/if}
</div>

<style>
  .cv { display: flex; flex-direction: column; min-height: 0; gap: 6px; }
  .files { display: flex; flex-wrap: wrap; gap: 6px; padding: 2px 0 4px; }
  .fchip { display: inline-flex; align-items: center; gap: 6px; max-width: 100%; padding: 3px 6px 3px 9px; border-radius: var(--radius); border: 1px solid var(--line2); font-size: 12px; color: var(--subtle); min-height: 28px; }
  .fchip:hover { background: var(--hover); color: var(--text); }
  .fchip.on { border-color: var(--acc); color: var(--text); background: var(--acc-dim); }
  .sc { height: 18px; font-size: 11px; padding: 0 6px 1px; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .fhead { display: flex; align-items: center; gap: 8px; padding: 0 4px 0 12px; }
  .rows { display: flex; flex-direction: column; }
  .sec { font-size: 11.5px; font-weight: 600; color: var(--muted); padding: 14px 12px 4px; }
  .sec .cap { font-weight: 400; }
  .tv { align-self: flex-start; margin-top: 6px; }
  .skels { display: flex; flex-direction: column; padding-top: 6px; }
  @media (max-width: 700px) { .fchip { min-height: 44px; } }
</style>
