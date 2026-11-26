// Words for mods the server's pack ships that have a newer file: the same
// as desktop's Mods tab (src/gpui/addons.rs pack_mods_label /
// pack_update_note), so both apps say it alike.

/**
 * Said in place of "Up to date" when only the pack's own mods have newer
 * files. `newer`: the pack's newer version ("8.6"), only when it has one.
 * `short`: the narrow bar's form.
 */
export function packModsLabel(n, newer, short = false) {
  if (short) return `Pack mods can update (${n})`;
  if (!newer) return `Pack mods can update separately (${n})`;
  const v = /^\d/.test(newer) ? `v${newer}` : newer;
  return `Pack mods can update separately (${n}) · pack ${v} is out`;
}

/** The confirm's sentence before one of the pack's own mods updates alone. */
export function packUpdateNote(ships) {
  return `The pack ships ${ships}. Players on the pack keep ${ships}; the server will differ until the pack updates.`;
}
