<script>
  import { onChange } from './lib/changes.svelte.js';
  // Mods › History, as desktop's History tab: what was removed, updated,
  // rolled back or edited on this server, newest first, each with Undo.
  // Undo downloads a mod again from CurseForge or Modrinth (History keeps
  // only where it came from); a config gets its old text back. When a mod's
  // version is gone from its site, a picker offers the ones it has.
  import { untrack } from 'svelte';
  import { ChevronDown, ChevronRight, History } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { bytes } from './lib/format.js';
  import { ago, dateOf } from './lib/modops.js';
  import { Selection } from './lib/selection.svelte.js';
  import SelectionBar from './lib/SelectionBar.svelte';
  import { paintDown, paintOver } from './lib/dragpick.svelte.js';

  let { server, onChanged = () => {} } = $props();
  const sid = $derived(server.id);
  let data = $state(null);
  let open = $state(new Set());
  let busy = $state(false);
  // The version picker, when an Undo's version is gone from its site.
  let pick = $state(null);
  let picked = $state(0);
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  // The clean up rules' numbers, as desktop's History footer shows them.
  const u = $derived(data?.usage);
  const cap = $derived(u ? u.max_mb * 1024 * 1024 : 0);
  const over = $derived(!!u && cap > 0 && u.limited > cap);

  async function load() {
    try {
      data = await api(`${base}/mod-backups`);
      // Rows are keyed by time and name: let go of ones that are gone.
      sel.retain(order);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function undo(set, file = null) {
    busy = true;
    try {
      const r = await post(`${base}/mod-backups/undo`, { when: set.when, name: file?.name ?? (set.files.length === 1 ? set.files[0].name : null) });
      if (r.restored.length) say(`Put back ${r.restored.join(', ')}.${r.restart_needed ? ' Restart the server to load it.' : ''}`);
      for (const e of r.errors) say(e, true);
      if (r.gone) {
        if (!r.gone.choices.length) say(`${r.gone.title}: ${r.gone.message}. It has no other version for this Minecraft and loader, so it can't be put back.`, true);
        else {
          picked = r.gone.closest;
          pick = { when: set.when, ...r.gone };
        }
      }
      await load();
      if (r.restored.length) onChanged();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }

  // Rows picked by click, Ctrl / Shift click, or press and drag across rows,
  // as desktop's History tab. Keys are "<when>:<file name>".
  const sel = new Selection();
  const keyOf = (s, f) => `${s.when}:${f.name}`;
  // A set's own key: a batch and loose files can share a time.
  const setKey = (s) => `${s.when}:${s.files[0]?.batch ?? ''}:${s.files[0]?.name ?? ''}`;
  // Who made a change, as desktop's History: "You" (who.name empty), a
  // panel user, or nobody known (older entries).
  const label = (w) => (w?.name ? w.name : 'You');
  const personOf = (w) => (w?.name ? w.id || `n:${w.name}` : 'you');
  const people = $derived.by(() => {
    const seen = new Map();
    for (const s of data?.sets ?? []) for (const f of s.files) if (f.who && !seen.has(personOf(f.who))) seen.set(personOf(f.who), f.who);
    return [...seen.entries()].sort((a, b) => (b[0] === 'you') - (a[0] === 'you'));
  });
  const faces = $derived(people.length > 0);
  // The person History shows; null is everyone.
  let whoPick = $state(null);
  const shown = (s) => whoPick === null || s.files.some((f) => f.who && personOf(f.who) === whoPick);
  function pickWho(k) {
    sel.clear();
    whoPick = k;
  }
  // Everyone in a set, once each.
  function setPeople(s) {
    const m = new Map();
    for (const f of s.files) if (f.who) m.set(personOf(f.who), f.who);
    return [...m.values()];
  }
  // "Updated from Robin's All the Mods 10", the name picked out to bold.
  function headParts(s) {
    const ps = setPeople(s);
    const who = ps.length ? label(ps[0]) : '';
    const at = who ? s.head.indexOf(who) : -1;
    return at < 0 ? [s.head, '', ''] : [s.head.slice(0, at), who, s.head.slice(at + who.length)];
  }
  // The rows shown, in order (a fold's files count once it is open).
  const order = $derived.by(() => {
    const out = [];
    for (const s of data?.sets ?? []) {
      if (!shown(s)) continue;
      if (s.files.length === 1) out.push(keyOf(s, s.files[0]));
      else if (open.has(setKey(s))) for (const f of s.files) out.push(keyOf(s, f));
    }
    return out;
  });
  // A press that started on a row, until the button is let go.
  let held = false;
  let start = null;
  const addKeys = (keys) => {
    const next = new Set(sel.keys);
    for (const k of keys) next.add(k);
    if (next.size !== sel.keys.size) sel.keys = next;
  };
  function setAt(i) {
    const k = order[i];
    if (k) addKeys(start && start !== k ? [start, k] : []);
  }
  function down(e, i) {
    if (e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey) return;
    held = true;
    start = order[i];
    window.addEventListener('pointerup', () => { held = false; start = null; }, { once: true, capture: true });
    paintDown(e, 'hist', i, true, () => {});
  }
  function rowClick(e, k) {
    sel.click(k, e, order);
  }
  // A fold: Ctrl / Shift click picks or lets go of its files; a drag over it picks them.
  const kidsOf = (s) => s.files.map((f) => keyOf(s, f));
  function headClick(e, s) {
    if (e.ctrlKey || e.metaKey || e.shiftKey) {
      const kids = kidsOf(s);
      const all = kids.every((k) => sel.has(k));
      const next = new Set(sel.keys);
      for (const k of kids) all ? next.delete(k) : next.add(k);
      sel.keys = next;
    } else flip(setKey(s));
  }
  function headEnter(e, s) {
    if (held && start && e.buttons & 1) addKeys([start, ...kidsOf(s)]);
  }
  const headPicked = (s) => s.files.every((f) => sel.has(keyOf(s, f)));
  // Undoes the picked rows, newest first, one after the other.
  async function undoPicked() {
    const rows = [];
    for (const s of data?.sets ?? []) for (const f of s.files) if (sel.has(keyOf(s, f)) && f.undo !== false) rows.push([s, f]);
    sel.clear();
    busy = true;
    const restored = [];
    let restart = false;
    try {
      for (const [s, f] of rows) {
        const r = await post(`${base}/mod-backups/undo`, { when: s.when, name: f.name });
        restored.push(...r.restored);
        restart ||= r.restart_needed;
        for (const e of r.errors) say(e, true);
        if (r.gone) {
          if (!r.gone.choices.length) say(`${r.gone.title}: ${r.gone.message}. It has no other version for this Minecraft and loader, so it can't be put back.`, true);
          else {
            picked = r.gone.closest;
            pick = { when: s.when, ...r.gone };
            break;
          }
        }
      }
      if (restored.length) say(`Put back ${restored.length} file${restored.length === 1 ? '' : 's'}.${restart ? ' Restart the server to load it.' : ''}`);
      await load();
      if (restored.length) onChanged();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }

  async function putBack() {
    const p = pick;
    const c = p?.choices[picked];
    pick = null;
    if (!c) return;
    busy = true;
    try {
      const r = await post(`${base}/mod-backups/put-back`, { when: p.when, name: p.name, file_id: c.id });
      say(`Put back ${r.restored.join(', ')}.${r.restart_needed ? ' Restart the server to load it.' : ''}`);
      await load();
      onChanged();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }

  // Asked first, never done by itself.
  async function free() {
    const body = `History holds ${bytes(data.copies)} of mod copies from older versions of Hopper. The ones CurseForge or Modrinth still hand out are removed; History keeps where to download each, so Undo still works. Copies with no download source stay.`;
    if (!(await confirmIt('Remove old copies of mods?', { body, ok: 'Free up space' }))) return;
    busy = true;
    try {
      const r = await post(`${base}/mod-backups/free`, {});
      say(!r.freed ? 'None of the copies can be downloaded again, so they stay.' : r.kept ? `Freed ${bytes(r.freed)}. ${bytes(r.kept)} stays: those mods have no download source.` : `Freed ${bytes(r.freed)}`);
      await load();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }

  // Runs the panel's clean up rules now (Settings › Panel folder).
  async function clean() {
    busy = true;
    try {
      const r = await post(`${base}/mod-backups/clean`, {});
      say(r.removed ? `Cleared ${r.removed} old History entr${r.removed === 1 ? 'y' : 'ies'} to the server's trash (${bytes(r.freed)})` : 'Nothing to clean up');
      await load();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }

  function flip(w) {
    const s = new Set(open);
    s.has(w) ? s.delete(w) : s.add(w);
    open = s;
  }

  const iso = (t) => new Date(t * 1000).toISOString();
  // "Today", "Yesterday", else the date, as desktop's day headers.
  function dayOf(t) {
    const d = new Date(t * 1000), now = new Date();
    const days = Math.round((new Date(now.toDateString()) - new Date(d.toDateString())) / 86400000);
    return days <= 0 ? 'Today' : days === 1 ? 'Yesterday' : dateOf(iso(t));
  }
  const hm = (t) => new Date(t * 1000).toTimeString().slice(0, 5);
  const tone = (c) => ({ Removed: 'red', Updated: 'blue', 'Rolled back': 'warn', 'Turned on': 'green', Added: 'green', 'Turned off': 'muted', Moved: 'blue', Copied: 'blue' })[c] ?? 'pri';
  // Sets grouped under their day.
  const days = $derived.by(() => {
    const out = [];
    for (const s of data?.sets ?? []) {
      if (!shown(s)) continue;
      const d = dayOf(s.when);
      if (out.at(-1)?.day !== d) out.push({ day: d, sets: [] });
      out.at(-1).sets.push(s);
    }
    return out;
  });
  const same = (s) => s.files.every((f) => f.change === s.files[0].change);

  $effect(() => {
    sid;
    untrack(load);
  });

  // Changed elsewhere (another tab, Hopper desktop, on disk): show it.
  $effect(() => onChange(sid, ['mods'], load));
</script>

{#snippet face(w, small)}
  {#if w}<span class="face" class:small class:you={!w.name} aria-hidden="true">{label(w).slice(0, 1).toUpperCase()}</span>{:else}<span class="face blank" class:small aria-hidden="true"></span>{/if}
{/snippet}

{#snippet entry(s, f, nested)}
  {@const k = keyOf(s, f)}
  {@const i = order.indexOf(k)}
  {@const w = nested ? null : f.who}
  <div class="row ent row-hover" class:nested class:row-sel={sel.has(k)} role="presentation" onpointerdown={(e) => down(e, i)} onpointerenter={(e) => paintOver(e, 'hist', i, setAt)} onclick={(e) => rowClick(e, k)}>
    {#if faces && !nested}{@render face(w, false)}{/if}
    <div class="col grow">
      <div class="row t">
        {#if w}<b class="who">{label(w)}</b><span class="verb">{f.verb}</span>{:else}<span class="ch {tone(f.change)}">{f.change}</span>{/if}
        {#if f.icon}<img src={f.icon} alt="" width="16" height="16" loading="lazy" />{/if}
        <span class="ell" title={f.name}>{f.title}</span>
      </div>
      <span class="tiny muted ell">{f.detail}</span>
    </div>
    {#if !nested}<span class="tiny muted" title={`${dateOf(iso(s.when))} · ${ago(iso(s.when))}`}>{hm(s.when)}</span>{/if}
    <!-- A move or copy between panels can't be undone from here: no Undo. -->
    {#if f.undo !== false}<button class="btn sm" disabled={busy} onclick={() => undo(s, nested ? f : null)}>Undo</button>{/if}
  </div>
{/snippet}

<div class="col bk">
  {#if data?.copies >= 1048576}
    <div class="row banner small">
      <span class="grow">Older History keeps full copies of mods ({bytes(data.copies)}). Most can be downloaded again instead.</span>
      <button class="btn sm pri" disabled={busy} onclick={free}>Free up space</button>
    </div>
  {/if}
  {#if !data}
    <div class="skels">{#each [0, 1, 2, 3] as i (i)}<div class="skel"><i></i></div>{/each}</div>
  {:else if !data.sets.length}
    <div class="empty-state"><span class="es-ic"><History size={20} /></span><b>Nothing yet</b><p>Mods you remove, update or roll back, and configs Hopper changes, show up here with Undo.</p></div>
  {:else}
    {#if people.length > 1}
      <div class="row whos" role="group" aria-label="Whose changes">
        <button class="chip wchip" class:on={whoPick === null} aria-pressed={whoPick === null} onclick={() => pickWho(null)}>Everyone</button>
        {#each people as [k, w] (k)}
          <button class="chip wchip" class:on={whoPick === k} aria-pressed={whoPick === k} onclick={() => pickWho(k)}>{@render face(w, true)}{label(w)}</button>
        {/each}
      </div>
    {/if}
    <div class="list">
      {#each days as d (d.day)}
        <div class="day tiny">{d.day}</div>
        {#each d.sets as s (setKey(s))}
          {#if s.files.length === 1}
            {@render entry(s, s.files[0], false)}
          {:else}
            {@const ps = setPeople(s)}
            {@const names = s.files.slice(0, 3).map((f) => f.title).join(', ') + (s.files.length > 3 ? '…' : '')}
            <div class="row ent row-hover" class:row-sel={headPicked(s)} role="presentation" onpointerdown={(e) => { if (e.button === 0 && !e.ctrlKey && !e.metaKey && !e.shiftKey) { held = true; start = null; window.addEventListener('pointerup', () => { held = false; start = null; }, { once: true, capture: true }); } }} onpointerenter={(e) => headEnter(e, s)}>
              <button class="nm grow" onclick={(e) => headClick(e, s)}>
                {#if faces}
                  {#if ps.length > 1}<span class="faces">{#each ps.slice(0, 3) as w, j (j)}{@render face(w, true)}{/each}</span>{:else}{@render face(ps[0], false)}{/if}
                {:else if open.has(setKey(s))}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}
                <span class="col grow">
                  {#if faces && s.head}
                    {@const [a, who, b] = headParts(s)}
                    <span class="row t"><span class="ell">{a}{#if who}<b class="who">{who}</b>{/if}{b}</span><span class="chev">{#if open.has(setKey(s))}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}</span></span>
                    <span class="tiny muted ell">{!same(s) || ps[0]?.pack ? `${s.summary} · ` : ''}{names}</span>
                  {:else}
                    <span class="row t"><span class="ch {same(s) ? tone(s.files[0].change) : 'muted'}">{same(s) ? s.files[0].change : 'Changed'}</span>{s.files.length} {s.files.every((f) => f.project_id) ? 'mods' : 'files'}{#if faces}<span class="chev">{#if open.has(setKey(s))}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}</span>{/if}</span>
                    <span class="tiny muted ell">{names}</span>
                  {/if}
                </span>
              </button>
              <span class="tiny muted">{hm(s.when)}</span>
              {#if s.files.some((f) => f.undo !== false)}<button class="btn sm" disabled={busy} onclick={() => undo(s)}>Undo all</button>{/if}
            </div>
            {#if open.has(setKey(s))}
              {#each s.files as f, j (j)}{@render entry(s, f, true)}{/each}
            {/if}
          {/if}
        {/each}
      {/each}
    </div>
  {/if}
  {#if sel.many}
    <SelectionBar n={sel.size} onClear={() => sel.clear()}>
      <button class="btn sm" disabled={busy} onclick={undoPicked}>Undo {sel.size}</button>
    </SelectionBar>
  {/if}
  <div class="row small foot">
    {#if u}
      <span class="muted">History uses {bytes(u.total)}{cap && !over ? ` of ${bytes(cap)}` : ''}</span>
      {#if over}<span class="chip warn">Over the {bytes(cap)} limit</span>{/if}
    {/if}
    <span class="sp"></span>
    {#if u}
      <span class="tiny muted">Keeps the last {u.keep_actions} actions and {u.keep_days} days</span>
      <button class="btn sm" disabled={busy || !u.removes} title="Clears History older than the rules in Settings › Panel folder (to the server's trash, Files › Trash). The panel does this by itself as History grows, but leaves older entries for you. World backups stay." onclick={clean}>{u.frees ? `Clean up · frees ${bytes(u.frees)}` : 'Clean up'}</button>
    {/if}
  </div>
</div>

{#if pick}
  <div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && (pick = null)} onkeydown={(e) => e.key === 'Escape' && (pick = null)}>
    <form class="box popover" onsubmit={(e) => { e.preventDefault(); putBack(); }}>
      <h3>Put back {pick.title}</h3>
      <p class="small">{pick.message}. Pick another for this server:</p>
      <div class="ol">
        {#each pick.choices as c, i (c.id)}
          <button type="button" class="row orow row-hover" class:row-sel={i === picked} onclick={() => (picked = i)}>
            <span class="grow ell">{c.version}</span>
            <span class="tiny muted">{[i === pick.closest ? 'closest' : i === 0 ? 'newest' : '', c.date ? dateOf(c.date) : ''].filter(Boolean).join(' · ')}</span>
          </button>
        {/each}
      </div>
      <div class="row end">
        <button type="button" class="btn" onclick={() => (pick = null)}>Cancel</button>
        <button class="btn pri">Put back {pick.choices[picked]?.version}</button>
      </div>
    </form>
  </div>
{/if}

<style>
  .bk { gap: 8px; }
  .ent { user-select: none; }
  .list { border: 1px solid var(--line); border-radius: var(--radius-lg); padding: 6px; }
  .day { padding: 8px 8px 4px; color: var(--subtle); font-weight: 600; text-transform: uppercase; letter-spacing: 0.04em; }
  .ent { gap: 10px; padding: 6px 8px; border-radius: var(--radius); }
  /* Under the head's text: past its face and gap. */
  .ent.nested { padding-left: 42px; }
  .t { gap: 6px; min-width: 0; }
  .ch { flex: none; font-weight: 500; }
  .ch.red { color: var(--red); }
  .ch.blue { color: var(--blue, #7fc3e8); }
  .ch.warn { color: var(--amber, #e0b04a); }
  .ch.pri { color: var(--accent); }
  .ch.muted { color: var(--subtle); }
  .ch.green { color: var(--green, #6cc788); }
  .nm { display: flex; align-items: center; gap: 10px; text-align: left; min-width: 0; }
  .who { font-weight: 600; }
  .verb { color: var(--subtle); flex: none; }
  .chev { display: inline-flex; color: var(--subtle); flex: none; }
  .whos { gap: 6px; flex-wrap: wrap; }
  .wchip { cursor: pointer; height: 26px; padding: 0 10px 2px 6px; font-size: 12px; gap: 6px; }
  .wchip:first-child { padding-left: 10px; }
  /* Who made a change: a panel user's initial (desktop shows a friend's head). */
  .face { flex: none; width: 24px; height: 24px; border-radius: var(--radius-sm); display: inline-flex; align-items: center; justify-content: center; font-size: 12px; font-weight: 600; color: var(--muted); background: #9a9aa333; }
  .face.you { color: var(--acc); background: var(--acc-dim); }
  .face.blank { background: none; }
  .face.small { width: 16px; height: 16px; font-size: 10px; border-radius: 4px; }
  .faces { display: inline-flex; width: 24px; flex: none; }
  .faces .face + .face { margin-left: -8px; box-shadow: 0 0 0 2px var(--bg-1, #17181c); }
  .grow { flex: 1; min-width: 0; }
  .col { display: flex; flex-direction: column; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .banner { gap: 12px; padding: 8px 12px; border-radius: var(--radius); background: var(--amber-dim, #e0b04a1a); }
  .foot { gap: 8px; padding-top: 4px; }
  .skels { display: flex; flex-direction: column; gap: 4px; }
  .box { width: min(460px, 100%); display: flex; flex-direction: column; gap: 12px; padding: 18px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .ol { border: 1px solid var(--line); border-radius: var(--radius); max-height: 280px; overflow: auto; display: flex; flex-direction: column; }
  .orow { gap: 10px; padding: 7px 12px; text-align: left; width: 100%; }
</style>
