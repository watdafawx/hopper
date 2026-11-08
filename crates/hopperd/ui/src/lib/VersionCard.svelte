<script>
  // One version in a changelog list, the same card everywhere (as in Hopper
  // desktop's versions.rs `version_card`): name and Release chip, then the
  // date and buttons, then the changelog. "Versions and changelogs" puts
  // every version in one; a mod's page puts the one it opened.
  let { name, release = '', date = '', installed = false, target = false, chips, actions, children } = $props();
</script>

<div class="vc" class:target class:cur={installed}>
  <div class="row wrap"><b class="vname">{name}</b>{#if release}<span class="chip">{release}</span>{/if}{#if installed}<span class="chip running">Installed</span>{/if}{@render chips?.()}</div>
  <div class="row wrap">
    <span class="tiny muted">{date}</span><span class="sp"></span>
    {@render actions?.()}
  </div>
  {@render children?.()}
</div>

<style>
  .vc { display: flex; flex-direction: column; gap: 6px; border: 1px solid var(--line); border-radius: var(--radius-lg); padding: 10px 12px; }
  .vc.target { border-color: var(--acc); }
  .vc.cur { background: #ffffff07; }
  .vname { overflow-wrap: anywhere; min-width: 0; }
  .wrap { flex-wrap: wrap; }
</style>
