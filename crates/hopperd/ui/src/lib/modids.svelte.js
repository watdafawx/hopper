// What a mod id from a crash ("arcane_framework" in "Needs arcane_framework
// 1.0") stands for on this server, so a card shows the mod's name and icon
// as Hopper desktop does, never the id. The panel answers (hopperd
// mods/for-ids, hopper-core modref): the jar the server has, the game or its
// loader, or the project a mod site lists under that id.
import { api } from './api.js';
import { LOADERS } from './modids.js';

/** Most ids the panel answers at once. */
const MOST = 12;

export class ModIds {
  /** id -> { kind: 'installed', file } | { kind: 'loader' } | { kind: 'project', source, id, name, icon, page } | { kind: 'unknown' } */
  refs = $state({});
  #asked = new Set();
  #seq = 0;

  /** Forgets everything (another server). */
  reset() {
    this.#seq++;
    this.#asked = new Set();
    this.refs = {};
  }

  /** Asks about the ids not asked yet (`again`: ask them even if so, e.g. after one was installed). */
  async ask(base, ids, again = false) {
    const want = [...new Set(ids.filter(Boolean))].filter((i) => again || !this.#asked.has(i)).slice(0, MOST);
    if (!want.length) return;
    want.forEach((i) => this.#asked.add(i));
    const n = this.#seq;
    try {
      const r = await api(`${base}/mods/for-ids?ids=${want.map(encodeURIComponent).join(',')}`);
      if (n === this.#seq) this.refs = { ...this.refs, ...(r.ids || {}) };
    } catch {
      // The card keeps showing the id, quietly; asking again later is fine.
      want.forEach((i) => this.#asked.delete(i));
    }
  }

  /** The name a card says for this id (empty until the panel has answered, or when nothing is found). */
  nameOf(id, modName) {
    const r = this.refs[id];
    if (!r) return LOADERS[id] || '';
    if (r.kind === 'loader') return LOADERS[id] || id;
    if (r.kind === 'project') return r.name;
    if (r.kind === 'installed') return modName?.(r.file) || '';
    return '';
  }
}
