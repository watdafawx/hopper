<script>
  // A description or changelog as hopperd sends it: hopper-core's parsed
  // blocks (text runs, http(s) links and images), never HTML. Everything
  // is set as text, so a pack page can't inject markup.
  // `compact`: a changelog, headings a step above the text (lib/Changelog).
  // `onProject(url)`: a link to a CurseForge or Modrinth project opens it
  // in the panel instead of a new tab.
  let { blocks = [], empty = 'Nothing written.', compact = false, onProject = null } = $props();

  const PROJECT = /^https?:\/\/(www\.)?(curseforge\.com\/(minecraft\/(mc-mods|modpacks|texture-packs|shaders|data-packs|worlds)|projects)|modrinth\.com\/(mod|modpack|resourcepack|shader|datapack|plugin|project))\/[^/?#]+/i;

  function click(e, href) {
    if (!onProject || !PROJECT.test(href) || e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    onProject(href);
  }

  // A size the page gave: its width, else its height (badges set only
  // that), never wider than the column.
  const imgW = (s) => (s.w ? `min(${s.w}px, 100%)` : null);
  const imgH = (s) => (!s.w && s.h ? `${s.h}px` : null);

  // A table of screenshots (every cell only images): cells share the width,
  // images without a size of their own fill their cell, and cells wrap
  // rather than shrink to icons. Sized images (badges) keep their size.
  const shots = (cells) => cells.every((c) => c.every((s) => s.img || !s.t.trim())) && cells.some((c) => c.some((s) => s.img));
  const fill = (s, grid) => grid && !s.w && !s.h;

  // A picture never grows past its own size, and a small one (a row of link
  // icons) stays that size and lines up with its neighbours instead of
  // filling the cell. Sizes are known once it has loaded.
  const SMALL = 160;
  function loaded(e) {
    const el = e.currentTarget;
    if (!el.classList.contains('fill')) return;
    if (el.naturalWidth < SMALL) el.dataset.small = '';
    else el.style.maxWidth = `${el.naturalWidth}px`;
  }
</script>

{#snippet spans(list, grid = false)}
  {#each list as s, i (i)}
    {#if s.img}
      {#if s.href}<a href={s.href} class:fill={fill(s, grid)} target="_blank" rel="noreferrer noopener" onclick={(e) => click(e, s.href)}><img src={s.img} alt={s.t} loading="lazy" onload={loaded} class:fill={fill(s, grid)} style:max-width={imgW(s)} style:height={imgH(s)} /></a>
      {:else}<img src={s.img} alt={s.t} loading="lazy" onload={loaded} class:fill={fill(s, grid)} style:max-width={imgW(s)} style:height={imgH(s)} />{/if}
    {:else if s.href}
      <a href={s.href} target="_blank" rel="noreferrer noopener" class:b={s.b} class:i={s.i} class:s={s.s} onclick={(e) => click(e, s.href)}>{s.t}</a>
    {:else if s.c}<code>{s.t}</code>
    {:else}<span class:b={s.b} class:i={s.i} class:s={s.s}>{s.t}</span>{/if}
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
      {#if shots(b.cells)}
        <div class="tr shots" style:grid-template-columns="repeat(auto-fit, minmax(min(160px, 100%), 1fr))">{#each b.cells.filter((c) => c.length) as c, j (j)}<div>{@render spans(c, true)}</div>{/each}</div>
      {:else}
        <div class="tr" style:grid-template-columns="repeat({b.cells.length}, minmax(0, 1fr))">{#each b.cells as c, j (j)}<div>{@render spans(c)}</div>{/each}</div>
      {/if}
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
  .s { text-decoration: line-through; }
  code { font: 12px var(--mono); background: var(--raise); padding: 0 5px; border-radius: var(--radius-sm); }
  pre { font: 12px var(--mono); background: var(--term); border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 10px; overflow: auto; margin: 0 0 8px; white-space: pre-wrap; }
  img { max-width: 100%; height: auto; border-radius: var(--radius-sm); vertical-align: middle; }
  hr { border: 0; border-top: 1px solid var(--line); margin: 12px 0; }
  .fill { display: block; width: 100%; }
  img.fill:global([data-small]), a.fill:has(> :global(img[data-small])) { display: inline-block; width: auto; }
  .tr { display: grid; gap: 10px; padding: 4px 0; border-bottom: 1px solid var(--line); font-size: 12.5px; }
  .tr.shots { border-bottom: 0; }
  .tr.shots > div { display: flex; flex-wrap: wrap; align-items: flex-start; gap: 8px; }
  .video { display: inline-flex; flex-direction: column; gap: 4px; margin: 4px 0 10px; }
  .video img { max-width: 360px; }
</style>
