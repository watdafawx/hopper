<script>
  // The palette button in the top bar, beside Settings, as in Hopper desktop
  // (src/gpui/appearance.rs): a popover with the colour themes. Resting on a
  // theme (or arrowing to it) shows it on the whole panel after a short
  // pause; a click keeps it; leaving, Esc or clicking outside puts the kept
  // one back. Zoom is the browser's own here (Ctrl + / Ctrl -), so it isn't
  // repeated.
  import { onMount, onDestroy, tick } from 'svelte';
  import { Palette, Check, ArrowRight } from '@lucide/svelte';
  import { THEMES, FOLLOW, theme, preview, choose } from './lib/theme.svelte.js';

  let { onSettings } = $props();

  // How long the pointer rests before the page shows a theme: sweeping across
  // the list shouldn't repaint everything.
  const DELAY = 150;

  let open = $state(false);
  let root = $state();
  let list = $state();
  let timer;

  const dark = THEMES.filter((t) => t.dark);
  const light = THEMES.filter((t) => !t.dark);
  const groups = [
    { title: 'Dark', items: dark },
    { title: 'Light', items: light },
  ];

  // What the pointer or focus rests on (null: nothing, so the kept theme).
  function want(name) {
    clearTimeout(timer);
    timer = setTimeout(() => {
      if ((theme.shown ?? null) !== name) preview(name);
    }, DELAY);
  }

  async function toggle() {
    if (open) return close();
    open = true;
    await tick();
    (list?.querySelector('[aria-checked="true"]') ?? list?.querySelector('button'))?.focus({ preventScroll: false });
  }

  function close() {
    clearTimeout(timer);
    if (theme.shown !== null) preview(null);
    open = false;
  }

  function pick(name) {
    clearTimeout(timer);
    choose(name);
  }

  function key(e) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      close();
      root?.querySelector('.pal-btn')?.focus();
      return;
    }
    const arrows = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -2, ArrowDown: 2 };
    if (!(e.key in arrows)) return;
    const all = [...list.querySelectorAll('button[role=radio]')];
    const i = all.indexOf(document.activeElement);
    if (i < 0) return;
    e.preventDefault();
    all[Math.max(0, Math.min(all.length - 1, i + arrows[e.key]))].focus();
  }

  function outside(e) {
    if (open && root && !root.contains(e.target)) close();
  }

  function shortcut(e) {
    if (e.ctrlKey && e.shiftKey && !e.altKey && e.key.toLowerCase() === 'a') {
      e.preventDefault();
      toggle();
    }
  }

  onMount(() => {
    window.addEventListener('pointerdown', outside, true);
    window.addEventListener('keydown', shortcut);
  });
  onDestroy(() => {
    clearTimeout(timer);
    window.removeEventListener('pointerdown', outside, true);
    window.removeEventListener('keydown', shortcut);
  });
</script>

<div class="ap" bind:this={root}>
  <button class="btn ghost icon pal-btn" class:on={open} aria-expanded={open} aria-haspopup="true" aria-label="Appearance" title="Appearance: theme (Ctrl+Shift+A)" onclick={toggle}><Palette size={14} /></button>
  {#if open}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div class="popover ap-pop" role="dialog" aria-label="Appearance" tabindex="-1" onkeydown={key}>
      <div class="ap-h"><b>Theme</b></div>
      <div class="ap-list" bind:this={list} role="radiogroup" aria-label="Theme" onpointerleave={() => want(null)}>
        <div class="ap-grid">
          <!-- svelte-ignore a11y_mouse_events_have_key_events -->
          <button class="ap-row row-hover" class:sel={theme.saved === FOLLOW} role="radio" aria-checked={theme.saved === FOLLOW}
            onpointerenter={() => want(FOLLOW)} onfocus={() => want(FOLLOW)} onclick={() => pick(FOLLOW)}>
            <span class="sw"><i style="background:#0e0e10"></i><i style="background:#fafafa"></i></span>
            <span class="nm">Follow system</span>{#if theme.saved === FOLLOW}<Check size={13} color="var(--text)" />{/if}
          </button>
        </div>
        {#each groups as g (g.title)}
          <div class="ap-sec">{g.title}</div>
          <div class="ap-grid">
            {#each g.items as t (t.name)}
              <!-- svelte-ignore a11y_mouse_events_have_key_events -->
              <button class="ap-row row-hover" class:sel={theme.saved === t.name} role="radio" aria-checked={theme.saved === t.name}
                onpointerenter={() => want(t.name)} onfocus={() => want(t.name)} onclick={() => pick(t.name)}>
                <span class="sw"><i style="background:{t.panel}"></i><i class="mid" style="background:{t.bg}"><b style="background:{t.accent}"></b><u style="background:{t.text}"></u></i></span>
                <span class="nm">{t.name}</span>{#if theme.saved === t.name}<Check size={13} color="var(--text)" />{/if}
              </button>
            {/each}
          </div>
        {/each}
      </div>
      <div class="ap-f"><button class="link" onclick={() => { close(); onSettings?.(); }}>All settings <ArrowRight size={12} /></button></div>
    </div>
  {/if}
</div>

<style>
  .ap { position: relative; display: inline-flex; }
  .ap-pop { position: absolute; top: calc(100% + 6px); right: -40px; z-index: 60; width: 340px; display: flex; flex-direction: column; }
  .ap-h { padding: 10px 12px 4px; font-size: 13px; }
  .ap-list { max-height: min(60vh, 420px); overflow-y: auto; padding: 0 6px 6px; }
  .ap-sec { padding: 8px 6px 2px; font-size: 11px; letter-spacing: .06em; text-transform: uppercase; color: var(--text); font-weight: 600; }
  .ap-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 2px; }
  .ap-row { display: flex; align-items: center; gap: 8px; height: 30px; padding: 0 8px; border-radius: var(--radius); text-align: left; font-size: 12.5px; min-width: 0; }
  .ap-row.sel { background: var(--sel); box-shadow: inset 3px 0 0 var(--acc); }
  .ap-row:focus-visible { outline: none; box-shadow: inset 0 0 0 2px var(--acc); }
  .nm { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sw { flex: none; display: inline-flex; width: 36px; height: 18px; border: 1px solid var(--line); border-radius: var(--radius-sm); overflow: hidden; }
  .sw i { display: flex; align-items: center; justify-content: center; gap: 2px; flex: 1; font-style: normal; }
  .sw i:first-child:not(.mid) { flex: none; width: 7px; }
  .sw .mid b { width: 5px; height: 5px; border-radius: 50%; }
  .sw .mid u { width: 8px; height: 2px; border-radius: 2px; }
  .ap-f { border-top: 1px solid var(--line); padding: 8px 12px; font-size: 12px; }
  .link { display: inline-flex; align-items: center; gap: 4px; color: var(--text); font-weight: 500; }
  .link:hover { text-decoration: underline; }
</style>
