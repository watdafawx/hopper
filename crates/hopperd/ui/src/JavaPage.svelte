<script>
  // Settings › Memory & Java: memory (two thumbs, -Xms and -Xmx), the Java,
  // desktop's flag generator, the Java arguments box and the files that
  // start the server, on one page. The generator runs in the panel (the
  // same code as Hopper desktop's), so a pick here writes the same flags.
  // Nothing saves from here: changes wait in Settings' save bar.
  import { FileCode, TriangleAlert, ChevronDown, ChevronRight, Info } from '@lucide/svelte';
  import MemoryRange from './MemoryRange.svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { flagsToText } from './lib/jvm.js';

  let { server, jvm, xms = $bindable(0), xmx = $bindable(4096), flagsText = $bindable(''), dirty = {} } = $props();

  /** The generator's rows, warnings and command line for what's on the page. */
  let view = $state(null);
  let showFlags = $state(false);
  let explained = $state(null);

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const host = $derived(jvm?.host_memory_mb || 65536);
  // The same range as desktop's server settings (the panel works it out):
  // 1 GB up to the PC's memory less 1 GB, at most 64 GB, in half gigabytes.
  const maxMb = $derived(Math.max(jvm?.memory_max_mb || Math.floor(Math.max(2048, Math.min(host - 1024, 65536)) / 512) * 512, xmx));
  const gb = (mb) => (mb >= 1024 ? `${(mb / 1024).toFixed(mb % 1024 ? 1 : 0)} GB` : `${mb} MB`);
  const memNote = $derived(xms > 0 ? `${gb(xms)} to ${gb(xmx)}` : gb(xmx));
  const lines = $derived(flagsText.split('\n').map((l) => l.trim()).filter((l) => l && !l.startsWith('#')));
  const typedMemory = $derived(lines.some((l) => /^-Xm[sx]/i.test(l)));
  const hasArgsFile = $derived((jvm?.files || []).includes('user_jvm_args.txt'));
  const extra = $derived(view?.extra || []);
  const rows = $derived(view?.rows || jvm?.rows || []);
  const tokens = $derived(view?.flags || []);

  let seq = 0;
  async function preview(pick) {
    const n = ++seq;
    try {
      const v = await api(`${base}/jvm/preview`, { method: 'POST', body: { xms_mb: xms, xmx_mb: xmx, flags_text: flagsText, ...(pick ? { pick } : {}) } });
      if (n !== seq) return;
      view = v;
      if (pick) flagsText = flagsToText(v.flags);
    } catch (e) {
      if (pick) say(e.message, true);
    }
  }

  // Typing and dragging update the preview after a short pause.
  let timer;
  $effect(() => {
    xms; xmx; flagsText;
    clearTimeout(timer);
    timer = setTimeout(() => preview(), 250);
    return () => clearTimeout(timer);
  });

  const pickRow = (row, value) => preview(row.kind === 'switch' ? { row: row.id, on: value } : { row: row.id, index: value });
</script>

<h4>Memory</h4>
<div class="fl" class:dirty={dirty.memory}>
  <div class="lab"><div>Memory</div><div class="tiny muted">The left end sets -Xms (memory claimed at start; at the very start Java picks), the right end -Xmx (the most it may use). Modded servers want 6 to 10 GB.{jvm.host_memory_mb ? ` This PC has ${gb(jvm.host_memory_mb)}.` : ''}</div></div>
  <div class="ctl"><MemoryRange bind:xms bind:xmx min={1024} max={maxMb} step={512} /><b class="memv">{memNote}</b></div>
</div>

<h4>Java</h4>
<div class="fl"><div class="lab"><div>Java</div><div class="tiny muted">{jvm.java_major ? `Java ${jvm.java_major}${view?.target?.graalvm ? ' (GraalVM)' : ''}` : 'Version unknown'}: the one the server starts with</div></div>
  <div class="ctl"><span class="mono small ell" title={jvm.java || ''}>{jvm.java || 'java on PATH'}</span></div></div>

<h4>Generator</h4>
<div class="tiny muted hint">Pick options and the panel writes the Java arguments below, the same way Hopper desktop does. Flags it doesn't cover stay after its own.</div>
<div class="gen">
  {#each rows as row, i (row.id)}
    {#if row.section && row.section !== rows[i - 1]?.section}<div class="sect">{row.section}</div>{/if}
    <div class="fl">
      <div class="lab"><div>{row.label}</div><div class="tiny muted">{row.note}</div></div>
      <div class="ctl">
        {#if row.kind === 'switch'}
          <label class="switch" title={row.on ? 'On' : 'Off'}><input type="checkbox" checked={row.on} aria-label={row.label} onchange={(e) => pickRow(row, e.currentTarget.checked)} /><span></span></label>
        {:else}
          <div class="seg" role="radiogroup" aria-label={row.label}>
            {#each row.labels as l, ix (ix)}<button role="radio" aria-checked={row.selected === ix} class:on={row.selected === ix} onclick={() => pickRow(row, ix)}>{l}</button>{/each}
          </div>
        {/if}
      </div>
    </div>
  {/each}
</div>

<h4>Java arguments ({lines.length}){#if extra.length}<span class="chip warn">Custom: {extra.length} flag{extra.length === 1 ? '' : 's'} the generator doesn't cover</span>{/if}</h4>
<div class="tiny muted hint">One per line, saved to user_jvm_args.txt. Edit them by hand or with the generator.{hasArgsFile ? '' : ' This server has no user_jvm_args.txt yet; its start script may not read one.'}</div>
<textarea class="flags mono" class:dirty={dirty.flags} spellcheck="false" rows={Math.min(24, Math.max(6, lines.length + 1))} bind:value={flagsText} aria-label="Java arguments"></textarea>
<div class="row tools">
  {#if tokens.length}<button class="btn sm ghost" onclick={() => { showFlags = !showFlags; explained = null; }}>{#if showFlags}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}Explain each</button>{/if}
  <span class="tiny muted">The memory slider sets -Xms and -Xmx.</span>
</div>
{#if showFlags}
  <div class="toks">{#each tokens as t, i (i)}<button class="tok mono" class:known={view?.explain?.[t]} class:on={explained === t} title={view?.explain?.[t] || "Hopper doesn't know this flag. It's kept as it is."} onclick={() => (explained = explained === t ? null : t)}>{t}</button>{/each}</div>
  {#if explained}<div class="explain row"><Info size={14} /><div><div class="mono">{explained}</div><div class="small muted">{view?.explain?.[explained] || "Hopper doesn't know this flag. It's kept as it is."}</div></div></div>{/if}
{/if}
{#if typedMemory}<div class="warnl"><TriangleAlert size={14} />-Xms or -Xmx here isn't used: the memory slider's are. Move the slider instead.</div>{/if}
{#each view?.conflicts || [] as w (w)}<div class="warnl"><TriangleAlert size={14} />{w}</div>{/each}

<h4>Command line</h4>
<div class="tiny muted hint">What the server starts with after saving: the start script with these arguments in it.</div>
<div class="cmd mono">{view?.command || 'No start script or server jar, so the panel can’t tell yet.'}</div>

<h4>Files that start the server</h4>
{#each jvm.files || [] as f (f)}
  <div class="fl"><div class="lab"><div class="mono">{f}</div><div class="tiny muted">{f === 'user_jvm_args.txt' ? 'Java arguments: memory and flags, written by this page' : 'The start script'}</div></div>
    <div class="ctl"><a class="btn sm" href="#/server/{encodeURIComponent(server.id)}/edit/{encodeURIComponent(f)}"><FileCode size={13} />Edit</a></div></div>
{:else}<div class="small muted">No run.bat, start.bat or user_jvm_args.txt; the server starts from its jar.</div>{/each}

<style>
  .hint { margin-bottom: 6px; }
  .fl { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 18px; align-items: center; padding: 10px 4px; border-bottom: 1px solid var(--line); }
  .fl.dirty { background: var(--acc-dim); border-radius: 6px; }
  .lab > div:first-child { font-weight: 500; }
  .ctl { display: flex; align-items: center; gap: 8px; justify-content: flex-end; min-width: 0; }
  .memv { min-width: 96px; text-align: right; white-space: nowrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 380px; display: block; }
  .gen { border: 1px solid var(--line); border-radius: 8px; padding: 0 12px; }
  .gen .fl:last-child { border-bottom: 0; }
  .sect { font-size: 11px; font-weight: 600; letter-spacing: .06em; text-transform: uppercase; color: var(--muted); padding: 12px 4px 0; }
  .seg { display: inline-flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; flex-wrap: wrap; }
  .seg button { padding: 3px 9px; font-size: 12px; color: var(--subtle); border-left: 1px solid var(--line2); }
  .seg button:first-child { border-left: 0; }
  .seg button:hover { background: var(--hover, #ffffff0d); }
  .seg button.on { background: var(--acc-dim); color: var(--text); }
  .chip { display: inline-block; margin-left: 8px; padding: 1px 8px; border-radius: 99px; font-size: 11px; text-transform: none; letter-spacing: 0; font-weight: 500; }
  .chip.warn { background: #eab30822; color: #eab308; }
  .flags { white-space: pre-wrap; word-break: break-all; font-size: 11.5px; background: var(--term); border: 1px solid var(--line); border-radius: 8px; padding: 10px 12px; max-height: 520px; overflow: auto; color: var(--text); width: 100%; resize: vertical; line-height: 1.5; }
  .flags:focus { outline: none; border-color: var(--acc); }
  .flags.dirty { border-color: #f9731677; }
  .tools { gap: 10px; margin-top: 6px; }
  .toks { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 8px; }
  .tok { font-size: 11px; padding: 2px 7px; border-radius: 5px; color: var(--muted); }
  .tok.known { border: 1px solid var(--line2); color: var(--text); }
  .tok.on { background: var(--acc-dim); }
  .explain { gap: 8px; align-items: flex-start; margin-top: 8px; color: var(--muted); }
  .explain .mono { color: var(--text); font-weight: 500; }
  .warnl { display: flex; gap: 6px; align-items: flex-start; color: #eab308; font-size: 13px; margin-top: 6px; }
  .warnl :global(svg) { flex: none; margin-top: 2px; }
  .cmd { font-size: 11.5px; background: var(--term); border: 1px solid var(--line); border-radius: 8px; padding: 10px 12px; color: var(--subtle); word-break: break-all; line-height: 1.5; }
  @media (max-width: 900px) { .fl { grid-template-columns: 1fr; } .ctl { justify-content: flex-start; flex-wrap: wrap; } }
</style>
