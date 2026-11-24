<script>
  // One notice for the page's "Needs you" chip (its list is <NoticeList>).
  // It shows nothing where it is written: it puts itself in the page's list
  // while it is mounted. `tier`: 'stop' (stops play; can't be closed),
  // 'choice' (needs a choice) or 'info'. Snippets get `fit` ({ narrow, lead };
  // the list's is never lead): `primary` is the main button (use
  // `mainClass(fit)`), `actions` the other buttons (under the row's Details),
  // `details` what opens under the row. `closeKey` closes it
  // in this browser until the key changes; `onclose` instead runs that.
  // `run`: 'stuck' or 'crashed' puts it in the run state's own chip instead (`minutes`: how long a
  // start has made no progress).
  import { onDestroy } from 'svelte';
  import { pageNotices, isClosed, close } from './notices.svelte.js';

  let { id, tier = 'info', title, subject = '', since = 0, icon = null, progress = null, closeKey = '', onclose = null, run = '', minutes = null, primary = null, actions = null, details = null } = $props();

  const page = pageNotices();
  const at = page?.next() ?? 0;
  const gone = $derived(isClosed(closeKey));
  const dismiss = $derived(tier === 'stop' ? null : onclose || (closeKey ? () => close(closeKey) : null));

  $effect(() => {
    if (!page) return;
    if (gone) page.drop(id);
    else page.put(id, { id, tier, title, subject, since, icon, progress, dismiss, run, minutes, primary, actions, details, at });
  });
  onDestroy(() => page?.drop(id));
</script>
