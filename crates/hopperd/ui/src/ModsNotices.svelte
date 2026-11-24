<script>
  // The Mods tab's notices on every tab of a server page, as desktop's: a mod in
  // twice, client-only mods on. ("Changed while it runs" is a chip in the header.) The list comes from the
  // Mods tab while it's open; on other tabs it's read here.
  import { untrack } from 'svelte';
  import Notice from './lib/Notice.svelte';
  import { mainClass } from './lib/noticeorder.js';
  import { api } from './lib/api.js';
  import { onChange } from './lib/changes.svelte.js';
  import { removeMods } from './lib/modops.js';
  import { modsShared } from './lib/modsShared.svelte.js';
  import { twiceOf, olderOf, clientOnOf, briefNames } from './lib/modsnotes.js';

  let { server, openMods } = $props();
  const shared = modsShared();
  const sid = $derived(server.id);
  const base = $derived(`/servers/${encodeURIComponent(sid)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  // The Mods tab is open: its list (it reloads it on every change).
  const tabOpen = $derived(!!shared?.list);

  let own = $state(null);
  async function load() {
    const b = base;
    try {
      const d = await api(`${b}/mods/list`);
      if (b === base) own = d;
    } catch {
      // The Mods tab says what went wrong; a notice can wait.
    }
  }
  // Read it here only while the Mods tab isn't open.
  $effect(() => {
    sid;
    if (!tabOpen) untrack(load);
  });
  $effect(() => (tabOpen ? undefined : onChange(sid, ['mods'], load)));

  const data = $derived(tabOpen ? shared.list() : own);
  const mods = $derived(data?.mods || []);
  const twice = $derived(twiceOf(mods));
  const older = $derived(olderOf(twice));
  const clientOn = $derived(clientOnOf(mods));
  const chip = $derived(tabOpen ? shared.chip?.() : null);

  async function keepNewest() {
    const reload = () => (shared?.reload ? shared.reload() : load());
    if (await removeMods(base, older, alive, { onUndo: reload })) reload();
  }
  const showClient = () => (shared?.show ? shared.show('client') : openMods('client'));
</script>

{#if twice.length}
  <!-- A mod in twice stops Fabric and Quilt from loading; NeoForge and Forge load one. -->
  <Notice id="mods-twice" tier={/^(fabric|quilt)/i.test(data?.loader || '') ? 'stop' : 'info'} title="{twice.length} mod{twice.length === 1 ? ' is' : 's are'} in twice" subject={briefNames(twice.map((l) => l[0].name))}>
    {#snippet primary(fit)}<button class={mainClass(fit)} title="Removes the older copies. Undo from History." onclick={keepNewest}>Keep newest</button>{/snippet}
  </Notice>
{/if}
{#if clientOn && chip !== 'client'}
  <Notice id="mods-client-on" tier="choice" title="{clientOn} client-only mod{clientOn === 1 ? '' : 's'} on" subject="Servers usually don't need {clientOn === 1 ? 'it' : 'them'}, and some won't start with them" closeKey="hopper.client-on.{sid}.{clientOn}">
    {#snippet primary(fit)}<button class={mainClass(fit)} title="Shows only the client-only mods" onclick={showClient}>Show</button>{/snippet}
  </Notice>
{/if}
