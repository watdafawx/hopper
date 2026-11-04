<script>
  // After a crashed start: the panel's read of the crash, laid out like
  // Hopper desktop's crash card (logs.rs crash_card, logs/crash_story.rs):
  // when a mod stopped the server from starting and the crash came after,
  // the fix first, then "What happened" in order; otherwise the headline
  // and each mod it names, most certain first, with Certain / Likely /
  // Possible and why. Mod names carry their icon and open the mod peek.
  // With `given` (a report picked in Crashes & logs: {crash, report, names,
  // lingered}) it shows that one instead, read-only: no fixes from an old
  // report.
  import { AlertTriangle, FileText, Share2, RefreshCw, Undo2, PowerOff, Trash2, Check, ChevronDown, ChevronRight } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { removeMods, setOn } from './lib/modops.js';
  import { relative } from './lib/worlds.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { whenLabel } from './lib/worlds.js';
  import CrashTurnOff from './CrashTurnOff.svelte';

  let { server, onPeek, onShare, openFile, given = null } = $props();

  const sid = $derived(server.id);
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  // Looked at again when the server stops or crashes (a new start's result).
  const settled = $derived(server.state === 'crashed' || server.state === 'stopped');
  let crash = $state(null);
  let report = $state(null);
  let lingered = $state(null);
  let mods = $state([]);
  let checking = $state(false);
  // Mods (jars) whose "Why?" list is open (what Hopper weighed for each).
  let whyOpen = $state({})
  // What was weighed, less what the sentence above already says.
  const moreFacts = (s) => (s.facts || []).filter((f) => !(s.why || '').includes(f.text));
  let busy = $state(false);
  // A change put back from this card: (jar now in mods, version back).
  let back = $state(null);
  // Only the newest look counts (the state can flip while one is out).
  let seq = 0;
  // "Turn off…" open: the mods it would turn off, to tick first.
  let picking = $state(false);

  async function load() {
    const n = ++seq;
    checking = true;
    try {
      const r = await api(`${base}/crash`);
      const m = r.crash ? (await api(`${base}/mods/list`)).mods || [] : [];
      if (n !== seq) return;
      if (crash?.when !== r.crash?.when) back = null;
      picking = false;
      crash = r.crash;
      report = r.report;
      lingered = r.lingered || null;
      mods = m;
    } catch {
      if (n === seq) crash = null;
    } finally {
      if (n === seq) checking = false;
    }
  }

  // A picked report: only the mods list is fetched (icons, on or off).
  async function loadGiven(g) {
    const n = ++seq;
    crash = g.crash;
    report = g.report;
    lingered = g.lingered || null;
    back = null;
    picking = false;
    try {
      const m = g.crash ? (await api(`${base}/mods/list`)).mods || [] : [];
      if (n === seq) mods = m;
    } catch {
      // names come from the report then, without icons
    }
  }

  $effect(() => {
    sid;
    if (given) loadGiven(given);
    else if (settled) load();
    else {
      seq++;
      crash = null;
      checking = false;
    }
  });

  const bare = (f) => (f || '').toLowerCase().replace(/\.disabled$/, '');
  const modOf = (file) => mods.find((m) => bare(m.file) === bare(file));
  const nameOf = (file, fallback = '') => modOf(file)?.name || given?.names?.[file] || fallback || (file || '').replace(/\.disabled$/, '').replace(/\.jar$/, '');
  const isOn = (file) => modOf(file)?.enabled !== false;

  const suspects = $derived(crash?.suspects || []);
  // The mod that stopped the server, when the crash came after it.
  const st = $derived(crash?.stopped && suspects.some((s) => s.file_name === crash.stopped.file_name) ? crash.stopped : null);
  const rootSuspect = $derived(st ? suspects.find((s) => s.file_name === st.file_name) : null);
  // The change the error names; once put back from here the crash no
  // longer names it, so the card keeps it to say so.
  const changed = $derived(rootSuspect?.changed || (st && back ? back.ch : null));
  // Certain and Likely, apart from errors that only followed from another mod's failure (knock-on: never turned off).
  const likely = $derived(suspects.filter((s) => s.certainty !== 'Possible' && !s.knock_on));
  // Turned off, or removed: Certain and Likely mods still on and here, apart from client-only ones (they go).
  const toTurnOff = $derived(likely.filter((s) => !s.client_only && modOf(s.file_name) && isOn(s.file_name)));
  const offAlready = $derived(likely.filter((s) => !s.client_only && modOf(s.file_name) && !isOn(s.file_name)));
  // The report's own error, after the mod that stopped loading.
  const knock = $derived(suspects.filter((s) => (s.knock_on || s.certainty === 'Possible') && s.file_name !== crash?.stopped?.file_name));
  const knockOn = $derived(knock.some((s) => s.knock_on));
  // The rest of the mods the crash names, beside the one that stopped it.
  const others = $derived(st ? suspects.filter((s) => s.file_name !== st.file_name) : []);
  const knockError = $derived((crash?.headline || '').split(' · ').slice(1).join(' · ') || crash?.headline || '');
  // A cause that isn't one mod, said by the crash itself, comes first.
  const note = $derived(
    crash?.advice ||
    {
      Memory: 'Java ran out of memory. Give the server more in its Java settings.',
      Native: 'Java itself crashed. Try another Java in the server\'s Java settings.',
      Unknown: "Hopper couldn't tell which mod. The crash report has the details.",
    }[crash?.kind] || null,
  );
  const versions = (ch) => (ch.from && ch.to ? ` (${ch.from} → ${ch.to})` : '');
  // Mod ids in a reason read as names.
  const named = (why) => suspects.reduce((w, s) => (s.mod_id && (modOf(s.file_name) || given?.names?.[s.file_name]) ? w.replaceAll(` ${s.mod_id} `, ` ${nameOf(s.file_name)} `).replaceAll(` ${s.mod_id},`, ` ${nameOf(s.file_name)},`).replaceAll(` ${s.mod_id}.`, ` ${nameOf(s.file_name)}.`) : w), why || '');

  // Only the mods ticked in the list "Turn off…" opened.
  async function turnOff(files) {
    busy = true;
    const ms = files.map(modOf).filter(Boolean);
    if (ms.length && (await setOn(base, ms, false, alive))) {
      picking = false;
      await load();
    }
    busy = false;
  }

  async function remove(s) {
    busy = true;
    if (await removeMods(base, [modOf(s.file_name)], alive)) await load();
    busy = false;
  }

  // Settings for the item the crash names that can turn just that off.
  const switches = $derived(crash?.switches || []);
  const switchedOff = $derived(crash?.switched_off || []);
  const fileOfMod = (id) => suspects.find((s) => s.mod_id === id)?.file_name;

  async function switchOff(sw) {
    const ok = await confirmIt(`Turn off ${sw.label}?`, {
      body: `Sets ${sw.key} to false in ${sw.file}, line ${sw.line}. The config as it was goes to Mods › Backups.`,
      ok: 'Turn off',
    });
    if (!ok) return;
    busy = true;
    try {
      await post(`${base}/crash/switch-off`, { file: sw.file, key: sw.key });
      say(`Turned off ${sw.label}. The old config is in Mods › Backups. Start the server to try again.`);
      await load();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }

  async function putBack(ch) {
    busy = true;
    try {
      const r = await post(`${base}/crash/put-back`, { file: ch.file_name });
      say(`${ch.from || 'The older version'} is back. ${ch.to || 'The newer one'} is in Mods › Backups.`);
      const keep = { file: r.back, from: ch.from, name: ch.name, ch };
      await load();
      back = keep;
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }
</script>

{#snippet mod(file, fallback = '')}
  {@const m = modOf(file)}
  {#if m}
    <button class="mod" title="Open {m.name}" onclick={() => onPeek(m)}>{#if m.icon}<img src={m.icon} alt="" width="18" height="18" loading="lazy" />{:else}<span class="ph"></span>{/if}<b>{m.name}</b></button>
  {:else}
    <span class="mod" title={file}><span class="ph"></span><b>{nameOf(file, fallback)}</b></span>
  {/if}
{/snippet}

{#snippet tag(c)}
  <span class="chip tag {c.toLowerCase()}">{c}</span>
{/snippet}

<!-- How sure, or "Knock-on" for an error that only followed from another mod's failure. -->
{#snippet sTag(s)}
  {#if s.knock_on}<span class="chip tag knockon" title="This error only followed from another mod's failure. Ignore it.">Knock-on</span>{:else}{@render tag(s.certainty)}{/if}
{/snippet}

<!-- The other mod a suspect's error is about, and what to do, in order. -->
{#snippet help(s)}
  {#if s.partner}
    <div class="line tiny partner"><span class="muted">Doesn't match</span>{@render mod(s.partner.file_name, s.partner.name)}{#if s.partner.version}<span class="muted">{s.partner.version} (installed)</span>{/if}</div>
  {/if}
  {#if s.steps?.length}
    <div class="todo">
      <span class="tiny muted b">What to do</span>
      <ol>{#each s.steps as t, k (k)}<li class="tiny"><span class="muted">{k + 1}.</span><span>{named(t)}</span></li>{/each}</ol>
    </div>
  {/if}
{/snippet}

{#snippet susRow(s)}
  <div class="sus" class:dim={s.knock_on}>
    <div class="who">{@render mod(s.file_name)}</div>
    <div class="grade">{@render sTag(s)}</div>
    <div class="why">
      <div class="tiny muted">{named(s.why)}</div>
      {#each s.needs || [] as [id, ver] (id)}<div class="tiny need">Needs {id}{ver ? ` ${ver}` : ''}</div>{/each}
      {@render help(s)}
      {#if (s.facts || []).length > 1 && moreFacts(s).length}
        <button class="btn ghost sm whybtn" aria-expanded={!!whyOpen[s.file_name]} onclick={() => (whyOpen[s.file_name] = !whyOpen[s.file_name])}>
          {#if whyOpen[s.file_name]}<ChevronDown size={12} />Hide why{:else}<ChevronRight size={12} />Why?{/if}
        </button>
        {#if whyOpen[s.file_name]}
          <ul class="facts">
            {#each moreFacts(s) as f, k (k)}
              <li><span class="mark" class:up={f.weight > 0} class:down={f.weight < 0}>{f.weight > 0 ? '+' : f.weight < 0 ? '−' : '·'}</span><span class="tiny muted">{named(f.text)}</span></li>
            {/each}
          </ul>
        {/if}
      {/if}
    </div>
    {#if !given && s.client_only && modOf(s.file_name)}
      <button class="btn sm" disabled={busy || alive} title={alive ? 'Stop the server first' : `Moves ${s.file_name} to .hopper-backups`} onclick={() => remove(s)}><Trash2 size={13} />Remove from server</button>
    {/if}
  </div>
{/snippet}

{#snippet picker()}
  {#if picking && toTurnOff.length}
    <CrashTurnOff suspects={toTurnOff} {mod} tag={sTag} {named} {nameOf} {busy} onConfirm={turnOff} onCancel={() => (picking = false)} />
  {/if}
{/snippet}

{#snippet actions()}
  {#if toTurnOff.length}
    {#if !picking}<button class="btn sm" disabled={busy} title="Shows which mods first, and why. Turn them back on in Mods." onclick={() => (picking = true)}><PowerOff size={13} />{toTurnOff.length === 1 ? `Turn off ${nameOf(toTurnOff[0].file_name)}…` : `Turn off ${toTurnOff.length} mods…`}</button>{/if}
  {:else if offAlready.length}
    <span class="done"><Check size={13} />{offAlready.length === 1 ? `${nameOf(offAlready[0].file_name)} is off` : `${offAlready.length} mods are off`}</span>
    <span class="tiny muted">Start the server to try again.</span>
  {/if}
{/snippet}

{#snippet switchRows()}
  {#each switches as sw, i (sw.file + sw.key)}
    <div class="sw">
      <div class="line">
        <span>{i === 0 && (toTurnOff.length || changed) ? 'Or keep' : 'Keep'}</span>{#if fileOfMod(sw.mod_id)}{@render mod(fileOfMod(sw.mod_id))}{:else}<b>{sw.mod_id}</b>{/if}<span>and turn off only the item that broke:</span><code>{sw.key} = true</code><span class="tiny muted">in {sw.file}, line {sw.line}</span>
      </div>
      <div class="row acts">
        <button class="btn sm" disabled={busy || alive} title={alive ? 'Stop the server first: it may write its config back when it stops.' : 'Sets it to false after you confirm. The old config goes to Backups.'} onclick={() => switchOff(sw)}><PowerOff size={13} />Turn off {sw.label}…</button>
        <button class="btn sm ghost" title="Opens the file in the editor" onclick={() => openFile(sw.file)}><FileText size={13} />Open config</button>
      </div>
    </div>
  {/each}
  {#each switchedOff as sw (sw.file + sw.key)}
    <div class="line"><span class="done"><Check size={13} />{sw.label} is off</span><span class="tiny muted">{sw.key} = false in {sw.file}. Start the server to try again.</span></div>
  {/each}
{/snippet}

{#if crash && (settled || given)}
  <div class="crash">
    <div class="row head">
      <AlertTriangle size={15} /><b>The server crashed</b><span class="muted when">{given ? whenLabel(crash.when) : relative(crash.when)}</span>
      <span class="grow"></span>
      {#if report}<button class="btn sm" title="Opens the crash report file" onclick={() => openFile(report)}><FileText size={13} />Open report</button>{/if}
      {#if !given}<button class="btn sm ghost icon" title="Look again" disabled={checking} onclick={load}><RefreshCw size={13} /></button>{/if}
    </div>

    {#if st}
      <div class="line">{@render mod(st.file_name)}<span>stopped the server from starting</span>{@render tag('Certain')}</div>
      {#if !given && (changed || toTurnOff.length || offAlready.length || switches.length || switchedOff.length)}
        <div class="fix">
          {#if changed}
            {@const isBack = back && back.name === changed.name}
            <div class="line strong">{@render mod(isBack ? back.file : changed.file_name)}<span>was updated {relative(changed.when)}{versions(changed)}</span></div>
            <div class="row acts">
              {#if isBack}
                <span class="done"><Check size={13} />{back.from || 'The older version'} is back</span>
                <span class="tiny muted">Start the server to try again. Mods › Backups has the newer one.</span>
              {:else}
                <button class="btn sm pri" disabled={busy || alive} title={alive ? 'Stop the server first' : 'Swaps in the copy Hopper kept. The newer one goes to Mods › Backups, so you can undo this there.'} onclick={() => putBack(changed)}><Undo2 size={13} />{changed.from ? `Put ${changed.from} back` : 'Put the older version back'}</button>
                {@render actions()}
                <span class="tiny muted">{changed.to ? `${changed.to} goes to Backups.` : 'The newer one goes to Backups.'}</span>
              {/if}
            </div>
            {@render picker()}
          {:else if toTurnOff.length || offAlready.length}
            <div class="row acts">{@render actions()}</div>
            {@render picker()}
          {/if}
          {@render switchRows()}
        </div>
      {/if}
      <b class="what">What happened</b>
      <ol class="steps">
        {#if changed}
          <li><span class="n">1</span><div class="line">{@render mod(back && back.name === changed.name ? back.file : changed.file_name)}<span>{changed.from && changed.to ? `was updated from ${changed.from} to ${changed.to}, ${relative(changed.when)}.` : `was updated ${relative(changed.when)}.`}</span></div></li>
        {/if}
        <li>
          <span class="n cause">{changed ? 2 : 1}</span>
          <div>
            <div class="line">{@render mod(st.file_name)}{@render tag('Certain')}<span>{st.error ? 'stopped loading with this error:' : 'failed while the server was loading.'}</span></div>
            {#if st.error}<div class="tiny muted quote">"{st.error}"</div>{/if}
            {#if rootSuspect}{@render help(rootSuspect)}{/if}
          </div>
        </li>
        {#if st.skipped?.length}
          <li><span class="n">{changed ? 3 : 2}</span><div class="line"><span>Loading went on broken. {st.skipped.length === 1 ? '1 mod that needs it was' : `${st.skipped.length} mods that need it were`} skipped:</span>{#each st.skipped as f (f)}{@render mod(f)}{/each}</div></li>
        {/if}
        {#if knock.length}
          <li class="knock">
            <span class="n">{1 + (changed ? 1 : 0) + 1 + (st.skipped?.length ? 1 : 0)}</span>
            <div>
              <div class="line"><span>Then the server crashed in</span>{#each knock as s (s.file_name)}{@render mod(s.file_name)}{/each}<span>{knockOn ? `(knock-on, ignore). That's the report's own error, which only followed from step ${changed ? 2 : 1}:` : "(Possible). That's the report's own error, a knock-on effect:"}</span></div>
              {#if knockError}<div class="tiny muted">{knockError}</div>{/if}
            </div>
          </li>
        {/if}
      </ol>
      {#if others.length}
        <b class="what">Other mods in this crash</b>
        <div class="rows">{#each others as s (s.file_name)}{@render susRow(s)}{/each}</div>
      {/if}
    {:else}
      {#if crash.headline}<div class="headline">{crash.headline}</div>{/if}
      {#if likely.length}
        <div class="line"><span class="muted">{likely.length === 1 ? 'Likely cause:' : 'Likely causes:'}</span><b>{likely.map((s) => nameOf(s.file_name)).join(', ')}</b></div>
      {/if}
      {#if suspects.length}
        <div class="tiny muted">Mods in this crash, most likely first:</div>
        <div class="rows">
          {#each suspects as s (s.file_name)}{@render susRow(s)}
          {/each}
        </div>
      {/if}
      {#if note}<div class="note">{note}</div>{/if}
      {#if !given && (toTurnOff.length || offAlready.length)}<div class="row acts">{@render actions()}</div>{@render picker()}{/if}
      {#if !given && (switches.length || switchedOff.length)}<div class="switches">{@render switchRows()}</div>{/if}
    {/if}

    {#if lingered}<div class="tiny muted">{lingered}</div>{/if}
    {#if report || crash.source}
      <div class="row acts share"><button class="btn sm" onclick={() => onShare(report || crash.source)}><Share2 size={13} />Share on mclo.gs</button></div>
    {/if}
  </div>
{/if}

<style>
  .crash { container-type: inline-size; display: flex; flex-direction: column; gap: 8px; padding: 14px 16px; margin-bottom: 10px; border: 1px solid #ef444499; border-radius: var(--radius-lg); background: #ef44440f; }
  .head { gap: 8px; color: var(--red); }
  .head b { font-weight: 600; font-size: 14px; }
  .when { font-size: 12.5px; }
  .grow { flex: 1; min-width: 0; }
  /* Words and mod names flow as one sentence that wraps anywhere. */
  .line { font-size: 13px; line-height: 22px; }
  .line > :global(*) { margin-right: 5px; vertical-align: middle; }
  .strong { font-weight: 600; }
  .headline { font-size: 13px; font-weight: 500; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .fix { display: flex; flex-direction: column; gap: 8px; padding: 12px; border-radius: var(--radius); border: 1px solid #f9731659; background: #f9731612; }
  .acts { gap: 6px; flex-wrap: wrap; align-items: center; }
  .share { padding-top: 2px; }
  .what { font-size: 13px; font-weight: 600; padding-top: 4px; }
  .steps { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 6px; }
  .steps li { display: flex; gap: 8px; align-items: flex-start; font-size: 13px; }
  .steps li > div { flex: 1; min-width: 0; }
  .n { flex: none; width: 20px; height: 20px; border-radius: 50%; display: inline-flex; align-items: center; justify-content: center; font-size: 11.5px; background: #a1a1aa33; color: var(--subtle); }
  .n.cause { background: var(--red); color: #fff; }
  .knock { color: var(--subtle); }
  .quote { white-space: normal; }
  .rows { display: flex; flex-direction: column; }
  .sus { display: flex; gap: 12px; align-items: flex-start; padding: 4px 0; }
  .who { flex: none; width: 14rem; min-width: 0; }
  .grade { flex: none; width: 5rem; }
  .why { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; overflow-wrap: anywhere; }
  .why .tiny, .quote, .steps .tiny { white-space: normal; overflow-wrap: anywhere; }
  .need { color: var(--gold); font-family: var(--mono, monospace); }
  .whybtn { align-self: flex-start; gap: 4px; padding: 0 4px; margin-left: -4px; }
  .facts { list-style: none; margin: 2px 0 0; padding: 6px 8px; display: flex; flex-direction: column; gap: 2px; border-radius: var(--radius); background: var(--raise); }
  .facts li { display: flex; gap: 8px; align-items: flex-start; }
  .mark { flex: none; width: 10px; font-size: 12px; color: var(--subtle); }
  .mark.up { color: var(--green); }
  .mark.down { color: var(--red); }
  .note { font-size: 13px; }
  .switches, .sw { display: flex; flex-direction: column; gap: 6px; }
  code { font-family: var(--mono, Consolas, monospace); font-size: 12px; color: var(--gold); }
  .mod { display: inline-flex; align-items: center; gap: 7px; flex: none; color: var(--text); padding: 0; max-width: 100%; }
  button.mod:hover b { color: var(--acc); }
  .mod img, .ph { width: 18px; height: 18px; border-radius: var(--radius-sm); background: var(--raise); flex: none; display: inline-block; }
  .mod b { font-weight: 600; font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tag { font-size: 11px; padding: 0 6px; }
  .tag.certain { color: var(--red); background: var(--red-dim); }
  .tag.likely { color: var(--gold); background: var(--gold-dim); }
  .tag.possible { color: var(--subtle); }
  .tag.knockon { color: var(--muted); }
  .dim { opacity: .7; }
  .partner { line-height: 20px; }
  .partner .mod b { font-size: 12px; }
  .todo { display: flex; flex-direction: column; gap: 1px; margin-top: 3px; }
  .todo .b { font-weight: 600; }
  .todo ol { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 1px; }
  .todo li { display: flex; gap: 6px; white-space: normal; overflow-wrap: anywhere; }
  .todo li > :first-child { flex: none; width: 1rem; }
  .done { display: inline-flex; align-items: center; gap: 5px; font-size: 12px; color: var(--green); border: 1px solid #22c55e55; border-radius: var(--radius); padding: 2px 8px; }
  /* A narrow card (beside the console): the reason goes under the name. */
  @container (max-width: 620px) {
    .sus { flex-wrap: wrap; row-gap: 2px; }
    .who { width: auto; max-width: calc(100% - 6rem); }
    .grade { width: auto; }
    .why { flex-basis: 100%; padding-left: 25px; }
  }
</style>
