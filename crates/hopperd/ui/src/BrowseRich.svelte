<script>
  // A description or changelog as hopperd sends it: hopper-core's parsed
  // blocks (text runs, http(s) links and images), never HTML. Everything
  // is set as text, so a pack page can't inject markup.
  // `compact`: a changelog, headings a step above the text (lib/Changelog).
  let { blocks = [], empty = 'Nothing written.', compact = false } = $props();
</script>

{#snippet spans(list)}
  {#each list as s, i (i)}
    {#if s.img}
      {#if s.href}<a href={s.href} target="_blank" rel="noreferrer noopener"><img src={s.img} alt={s.t} loading="lazy" style:max-width={s.w ? `min(${s.w}px, 100%)` : null} /></a>
      {:else}<img src={s.img} alt={s.t} loading="lazy" style:max-width={s.w ? `min(${s.w}px, 100%)` : null} />{/if}
    {:else if s.href}
      <a href={s.href} target="_blank" rel="noreferrer noopener" class:b={s.b} class:i={s.i}>{s.t}</a>
    {:else if s.c}<code>{s.t}</code>
    {:else}<span class:b={s.b} class:i={s.i}>{s.t}</span>{/if}
  {/each}
{/snippet}

<div class="rich" class:compact>
  {#each blocks as b, i (i)}
    {#if b.k === 'h'}
      <div class="h h{Math.min(b.l || 2, 4)}">{@render spans(b.s)}</div>
    {:else if b.k === 'li' || b.k === 'ol'}
      <div class="li" style:padding-left="{(b.d || 0) * 18 + 18}px"><span class="mk">{b.k === 'li' ? '•' : '–'}</span>{@render spans(b.s)}</div>
    {:else if b.k === 'hr'}
      <hr />
    {:else if b.k === 'code'}
      <pre>{b.s.map((s) => s.t).join('')}</pre>
    {:else if b.k === 'row'}
      <div class="tr" style:grid-template-columns="repeat({b.cells.length}, minmax(0, 1fr))">{#each b.cells as c, j (j)}<div>{@render spans(c)}</div>{/each}</div>
    {:else if b.k === 'video'}
      {#if b.url}<a class="video" href={b.url} target="_blank" rel="noreferrer noopener">{#if b.thumb}<img src={b.thumb} alt="" loading="lazy" />{/if}<span>▶ Watch {b.site === 'video' ? 'the video' : `on ${b.site}`}</span></a>{/if}
    {:else}
      <p>{@render spans(b.s)}</p>
    {/if}
  {:else}
    <p class="muted">{empty}</p>
  {/each}
</div>

<style>
  .rich { color: #d4d4da; font-size: 13px; line-height: 1.6; overflow-wrap: anywhere; }
  .rich p { margin: 0 0 8px; }
  .h { color: var(--text); font-weight: 650; margin: 14px 0 6px; }
  .h1 { font-size: 18px; } .h2 { font-size: 16px; } .h3 { font-size: 14.5px; } .h4 { font-size: 13.5px; }
  .compact .h { font-size: 13px; margin: 10px 0 4px; }
  .compact .h1, .compact .h2 { font-size: 14px; }
  .compact .h:first-child { margin-top: 0; }
  .li { position: relative; margin: 2px 0; }
  .mk { position: absolute; margin-left: -14px; color: var(--muted); }
  .b { font-weight: 650; color: var(--text); }
  .i { font-style: italic; }
  code { font: 12px var(--mono); background: var(--raise); padding: 0 5px; border-radius: var(--radius-sm); }
  pre { font: 12px var(--mono); background: var(--term); border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 10px; overflow: auto; margin: 0 0 8px; white-space: pre-wrap; }
  img { max-width: 100%; height: auto; border-radius: var(--radius-sm); vertical-align: middle; }
  hr { border: 0; border-top: 1px solid var(--line); margin: 12px 0; }
  .tr { display: grid; gap: 10px; padding: 4px 0; border-bottom: 1px solid var(--line); font-size: 12.5px; }
  .video { display: inline-flex; flex-direction: column; gap: 4px; margin: 4px 0 10px; }
  .video img { max-width: 360px; }
</style>
