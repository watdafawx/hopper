// Mod ids from a crash ("arcane_framework" in "Needs arcane_framework 1.0")
// read as names. The panel's answers are kept in modids.svelte.js.

/** The game and the loaders, as a player says them. */
export const LOADERS = {
  minecraft: 'Minecraft',
  neoforge: 'NeoForge',
  forge: 'Forge',
  fml: 'Forge',
  fml_loader: 'Forge',
  fabricloader: 'Fabric Loader',
  quilt_loader: 'Quilt Loader',
  java: 'Java',
};

/**
 * `text` with each id in `names` (a Map of id -> name) read as its name. Only
 * the whole id counts: not inside a longer word, id or path, and a full stop
 * that ends the sentence isn't part of it.
 */
export function readIds(text, names) {
  let out = text || '';
  for (const [id, name] of names) {
    if (!id || !name) continue;
    const esc = id.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    out = out.replace(new RegExp(String.raw`(?<![\w.-])${esc}(?![\w-]|\.\w)`, 'g'), () => name);
  }
  return out;
}
