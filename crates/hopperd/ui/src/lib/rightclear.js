// Right-clicking a search or filter field empties it, like the search boxes
// in Minecraft's creative inventory and JEI. Players expect it there, so the
// clear wins over the browser's copy / paste menu. Firing `input` makes it
// behave like deleting the text by hand: bind:value and any oninput handler
// (a re-search, a refilter) see the change.
export function rightClear(node) {
  const clear = (e) => {
    e.preventDefault();
    if (!node.value) return;
    node.value = '';
    node.dispatchEvent(new Event('input', { bubbles: true }));
  };
  node.addEventListener('contextmenu', clear);
  return { destroy: () => node.removeEventListener('contextmenu', clear) };
}
