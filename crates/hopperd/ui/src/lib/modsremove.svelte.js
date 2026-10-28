// The Mods area's remove question and its Undo toast, as in Hopper desktop:
// "Remove X?" with the mods that need it, "Also remove these mods? Nothing
// else needs them", and "Removed X" with Undo. ModsRemove.svelte draws
// them; lib/modops.js's removeMods asks through here when one is on the
// page and falls back to the plain question when none is.

export const removing = $state({
  /** ModsRemove components on the page. */
  hosts: 0,
  /** The open question: { title, need, orphans: [{ file, name, icon }], resolve }. */
  ask: null,
  /** The last remove's Undo: { base, token, text, alive, onUndo, at }. */
  undo: null,
});

/** Asks; resolves to the extra files to remove too ([] for none), or null when cancelled. */
export function askRemove(title, need, orphans) {
  removing.ask?.resolve(null);
  return new Promise((resolve) => (removing.ask = { title, need, orphans, resolve }));
}

export function answerRemove(v) {
  const a = removing.ask;
  removing.ask = null;
  a?.resolve(v);
}

/** Offers Undo for a remove the server can put back. */
export function offerUndo(u) {
  removing.undo = { ...u, at: Date.now() };
}
