<script>
  // One notice for the page's slot above the tabs (<NoticeSlot>). It shows
  // nothing where it is written: it puts itself in the page's list while it
  // is mounted. `tier`: 'stop' (stops play; can't be closed), 'choice'
  // (needs a choice) or 'info'. Snippets get `fit` ({ narrow, lead }):
  // `primary` is the main button (filled only on the slot's first strip:
  // use `mainClass(fit)`), `actions` the other buttons (tucked into Details
  // when narrow), `details` what opens under the strip. `closeKey` closes it
  // in this browser until the key changes; `onclose` instead runs that.
  import { onDestroy } from 'svelte';
  import { pageNotices, isClosed, close } from './notices.svelte.js';

  let { id, tier = 'info', title, subject = '', since = 0, icon = null, progress = null, closeKey = '', onclose = null, primary = null, actions = null, details = null } = $props();

  const page = pageNotices();
  const at = page?.next() ?? 0;
  const gone = $derived(isClosed(closeKey));
  const dismiss = $derived(tier === 'stop' ? null : onclose || (closeKey ? () => close(closeKey) : null));

  $effect(() => {
    if (!page) return;
    if (gone) page.drop(id);
    else page.put(id, { id, tier, title, subject, since, icon, progress, dismiss, primary, actions, details, at });
  });
  onDestroy(() => page?.drop(id));
</script>
