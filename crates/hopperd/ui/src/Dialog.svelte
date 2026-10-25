<script>
  import { dialog, answer } from './lib/dialog.svelte.js';

  let field = $state();
  let okBtn = $state();

  $effect(() => {
    if (dialog.open && !dialog.input && okBtn) okBtn.focus();
    if (dialog.open && dialog.input && field) {
      field.focus();
      // Select the name without its extension, like Explorer.
      const v = field.value;
      const dot = v.lastIndexOf('.');
      field.setSelectionRange(0, dot > 0 ? dot : v.length);
    }
  });

  function submit(e) {
    e.preventDefault();
    answer(dialog.input ? dialog.input.value : true);
  }

  function keys(e) {
    if (e.key === 'Escape') answer(dialog.input ? null : false);
  }
</script>

{#if dialog.open}
  <div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && answer(dialog.input ? null : false)} onkeydown={keys}>
    <form class="box card" onsubmit={submit}>
      <h3>{dialog.title}</h3>
      {#if dialog.body}<p class="small subtle">{dialog.body}</p>{/if}
      {#if dialog.input}
        <div class="input"><input bind:this={field} bind:value={dialog.input.value} placeholder={dialog.input.placeholder} spellcheck="false" autocomplete="off" /></div>
      {/if}
      <div class="row end">
        <button type="button" class="btn" onclick={() => answer(dialog.input ? null : false)}>Cancel</button>
        {#if dialog.alt}<button type="button" class="btn" onclick={() => answer('alt')}>{dialog.alt}</button>{/if}
        <button class="btn" class:pri={!dialog.danger} class:bad={dialog.danger} bind:this={okBtn}>{dialog.ok}</button>
      </div>
    </form>
  </div>
{/if}

<style>
  .back { position: fixed; inset: 0; background: #0008; display: grid; place-items: center; z-index: 60; padding: 16px; }
  .box { width: min(420px, 100%); display: flex; flex-direction: column; gap: 12px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0; font-size: 14px; }
  .end { justify-content: flex-end; }
</style>
