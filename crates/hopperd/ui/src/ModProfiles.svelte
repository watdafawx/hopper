<script>
  // Profiles ▾, as in Hopper desktop's Mods toolbar: switch to a saved set
  // of mods that are off, save the current one, delete one, or start
  // finding the mod that breaks the server.
  import { Layers, ChevronDown, Plus, Trash2 } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { ask, confirmIt } from './lib/dialog.svelte.js';

  let { base, alive = false, onChanged = () => {} } = $props();
  let open = $state(false);
  let list = $state(null);

  async function toggleOpen(e) {
    e.stopPropagation();
    open = !open;
    if (open) {
      list = null;
      try {
        list = (await api(`${base}/mods/profiles`)).profiles;
      } catch (err) {
        list = [];
        say(err.message, true);
      }
    }
  }

  const what = (p) => {
    if (!p.turns_on && !p.turns_off) return 'Matches what is on now';
    const part = (n, names, verb) => (n ? `${verb} ${n}: ${names.join(', ')}${n > names.length ? ` +${n - names.length}` : ''}` : '');
    return [part(p.turns_off, p.off_names, 'Turns off'), part(p.turns_on, p.on_names, 'Turns on')].filter(Boolean).join('\n');
  };

  async function use(p) {
    open = false;
    try {
      const r = await post(`${base}/mods/profiles/use`, { name: p.name });
      say(`Switched to “${p.name}”: ${r.changed} mod${r.changed === 1 ? '' : 's'} changed.${r.restart_needed ? ' Restart the server to load them.' : ''}`);
      onChanged();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function save() {
    open = false;
    const name = await ask('Save mod profile', '', {
      body: 'Remembers which mods are off now, to switch back to later. A profile with the same name is replaced.',
      ok: 'Save',
      placeholder: 'Profile name',
    });
    if (!name?.trim()) return;
    try {
      await post(`${base}/mods/profiles/save`, { name: name.trim() });
      say(`Saved profile “${name.trim()}”.`);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function remove(p, e) {
    e.stopPropagation();
    open = false;
    if (!(await confirmIt(`Delete profile “${p.name}”?`, { body: 'Only the saved list goes; no mod is turned on or off.', ok: 'Delete', danger: true }))) return;
    try {
      await post(`${base}/mods/profiles/delete`, { name: p.name });
      say(`Deleted profile “${p.name}”.`);
    } catch (err) {
      say(err.message, true);
    }
  }
</script>

<svelte:window onclick={() => (open = false)} onkeydown={(e) => e.key === 'Escape' && open && (open = false)} />

<div class="wrapp">
  <button class="btn sm" onclick={toggleOpen} title="Switch which mods are on"><Layers size={13} />Profiles<ChevronDown size={12} /></button>
  {#if open}
    <div class="pop popover menu" role="menu" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Escape' && (open = false)}>
      <div class="hd">Switch which mods are enabled</div>
      {#if list === null}
        <div class="hd row"><span class="spinner"></span>Loading…</div>
      {:else}
        {#each list as p (p.name)}
          <div class="it row-hover">
            <button class="use menu-item" title={what(p)} onclick={() => use(p)}>
              <span class="ell">Use “{p.name}”</span>
              {#if !p.turns_on && !p.turns_off}<span class="chip running">now</span>{:else}<span class="tiny muted">{p.turns_off ? `−${p.turns_off}` : ''}{p.turns_on && p.turns_off ? ' ' : ''}{p.turns_on ? `+${p.turns_on}` : ''}</span>{/if}
            </button>
            <button class="ib" title="Delete this profile" onclick={(e) => remove(p, e)}><Trash2 size={12} /></button>
          </div>
        {:else}
          <div class="hd">No profiles yet.</div>
        {/each}
      {/if}
      <div class="menu-sep"></div>
      <button class="menu-item" onclick={save}><Plus size={13} />Save current as…</button>
      {#if alive}<div class="hd">Switching while it runs: restart to load the change.</div>{/if}
    </div>
  {/if}
</div>

<style>
  .wrapp { position: relative; }
  .pop { position: absolute; z-index: 70; top: calc(100% + 4px); left: 0; width: 280px; display: flex; flex-direction: column; }
  .hd { color: var(--muted); font-size: 11.5px; padding: 5px 10px; }
  .it { display: flex; align-items: center; gap: 2px; border-radius: var(--radius); }
  .use { flex: 1; min-width: 0; }
  .it .use:hover { background: none; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; min-width: 0; }
  .ib { color: var(--muted); padding: 4px 6px; border-radius: 5px; display: grid; }
  .ib:hover { color: var(--red); background: #ffffff10; }
</style>
