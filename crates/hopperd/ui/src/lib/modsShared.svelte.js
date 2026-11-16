// What a server page and its Mods tab share, so the Mods tab's notices show
// on every tab (as desktop's) without reading the list twice: while the Mods
// tab is open it hands over its list, its filter chip and its reload; a
// notice on another tab can ask it to open on a chip.
import { getContext, setContext } from 'svelte';

const KEY = Symbol('mods-shared');

export function provideModsShared() {
  const s = $state({ list: null, chip: null, show: null, reload: null, want: null });
  setContext(KEY, s);
  return s;
}

/** The page's shared state, or null outside a server page. */
export const modsShared = () => getContext(KEY) ?? null;
