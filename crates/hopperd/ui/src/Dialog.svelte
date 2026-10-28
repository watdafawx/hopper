<script>
  import { dialog, answer } from './lib/dialog.svelte.js';

  let field = $state();
  let okBtn = $state();
  let box = $state();
  // What had focus before the dialog opened: it gets it back after.
  let before = null;

  $effect(() => {
    if (!dialog.open) return;
    before = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    if (!dialog.input && okBtn) okBtn.focus();
    if (dialog.input && field) {
      field.focus();
      // Select the name without its extension, like Explorer.
      const v = field.value;
      const dot = v.lastIndexOf('.');
      field.setSelectionRange(0, dot > 0 ? dot : v.length);
    }
    return () => {
      const b = before;
      before = null;
      if (b?.isConnected) b.focus();
    };
  });

  function submit(e) {
    e.preventDefault();
    answer(dialog.input ? dialog.input.value : true);
  }

  const cancel = () => answer(dialog.input ? null : false);

  // On the window, so Escape works wherever focus went (a click on the
  // text drops it to the page), and Tab stays inside the dialog.
  function keys(e) {
    if (!dialog.open) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      cancel();
    } else if (e.key === 'Tab' && box) {
      const els = [...box.querySelectorAll('button:not(:disabled), input:not(:disabled)')];
      if (!els.length) return;
      const [first, last] = [els[0], els[els.length - 1]];
      if (!box.contains(document.activeElement)) {
        e.preventDefault();
        first.focus();
      } else if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  }
</script>

<svelte:window onkeydowncapture={keys} />

{#if dialog.open}
  <!-- Opens at the shared dialog height under the title bar, like desktop's dialogs. -->
  <div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && cancel()}>
    <div class="box popover" role="dialog" aria-modal="true" aria-label={dialog.title} bind:this={box}>
    <form class="f" onsubmit={submit}>
      <h3>{dialog.title}</h3>
      {#if dialog.body}<p class="small subtle">{dialog.body}</p>{/if}
      {#if dialog.input}
        <div class="input"><input bind:this={field} bind:value={dialog.input.value} placeholder={dialog.input.placeholder} spellcheck="false" autocomplete="off" /></div>
      {/if}
      <div class="row end">
        <button type="button" class="btn" onclick={cancel}>Cancel</button>
        {#if dialog.alt}<button type="button" class="btn" onclick={() => answer('alt')}>{dialog.alt}</button>{/if}
        <button class="btn" class:pri={!dialog.danger} class:danger={dialog.danger} bind:this={okBtn}>{dialog.ok}</button>
      </div>
    </form>
    </div>
  </div>
{/if}

<style>
  .dialog-back { z-index: 80; }
  .box { width: min(440px, 100%); padding: 18px; }
  .f { display: flex; flex-direction: column; gap: 12px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .end { justify-content: flex-end; }
</style>
