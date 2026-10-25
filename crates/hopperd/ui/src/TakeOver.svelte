<script>
  // The strip under a Crafty server's header, and its confirm dialog:
  // Hopper takes the server over, keeping Crafty's start command.
  import { Info, X, TriangleAlert } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let { server } = $props();
  let open = $state(false);
  let st = $state(null);
  let busy = $state(false);
  let loading = $state(false);

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const gb = (mb) => (mb >= 1024 ? `${(mb / 1024).toFixed(mb % 1024 ? 1 : 0)} GB` : `${mb} MB`);
  /** The start command without its Java, which has its own row. */
  const args = $derived.by(() => {
    const c = st?.crafty?.command?.trim() || '';
    const rest = c.startsWith('"') ? c.slice(c.indexOf('"', 1) + 1) : c.slice(c.search(/\s|$/));
    return rest.replace(/%\*\s*$/, '').trim();
  });
  const craftyJava = $derived.by(() => {
    const c = st?.crafty?.command?.trim() || '';
    return c.startsWith('"') ? c.slice(1, c.indexOf('"', 1)) : c.split(/\s/)[0];
  });
  const blocked = $derived(!st || !st.crafty || !!st.busy || !!st.plan?.error);

  async function show() {
    open = true;
    loading = true;
    st = null;
    try {
      st = await api(`${base}/takeover`);
    } catch (e) {
      say(e.message, true);
      open = false;
    } finally {
      loading = false;
    }
  }

  async function take() {
    busy = true;
    try {
      const r = await post(`${base}/takeover`);
      open = false;
      say(`Hopper runs ${server.name} now.${r.notes?.length ? ' ' + r.notes[r.notes.length - 1] : ''}`);
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }

  function keys(e) {
    if (e.key === 'Escape' && !busy) open = false;
  }
</script>

<div class="strip">
  <Info size={14} />
  <span><b>Crafty runs this server</b>, so Hopper won't start it. Take it over to run it here; it keeps Crafty's start command, and you can give it back.</span>
  <span class="sp"></span>
  <button class="btn sm pri" onclick={show}>Take over…</button>
</div>

{#if open}
  <div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && (open = false)} onkeydown={keys}>
    <div class="box card col">
      <h3>Take {server.name} over from Crafty?<span class="sp"></span><button class="btn sm ghost icon" title="Close" disabled={busy} onclick={() => (open = false)}><X size={14} /></button></h3>
      <p class="small subtle">Hopper Server will start, stop and back it up from now on. Nothing in the world or mods changes.</p>
      {#if loading}
        <div class="small muted">Reading Crafty's settings…</div>
      {:else if st && !st.crafty}
        <div class="err small">{st.crafty_error || "Crafty's settings for this server weren't found."}</div>
      {:else if st}
        <div class="kv small">
          <span class="muted">Start command</span><span class="mono">{args}</span>
          <span class="muted">Java</span>
          <span>
            {#if st.plan?.error}<span class="err">{st.plan.error}</span>
            {:else if st.plan?.java}Crafty uses plain <span class="mono">java</span> → <b>Java {st.plan.java_major ?? '?'}</b>{#if st.plan.minecraft}{` for Minecraft ${st.plan.minecraft}`}{/if}<div class="mono tiny muted ell" title={st.plan.java}>{st.plan.java}</div>
            {:else}<span class="mono ell" title={craftyJava}>{craftyJava}</span> <span class="muted">(kept)</span>{/if}
          </span>
          <span class="muted">Port</span><span>{st.crafty.port || server.port} (unchanged)</span>
          {#if st.plan?.memory_mb}<span class="muted">Memory</span><span>{gb(st.plan.memory_mb)}</span>{/if}
        </div>
        <div class="chg small"><b>Changes in the folder</b>
          <ul><li>run.bat gets Crafty's command (an old run.bat is kept as run.bat.before-hopper)</li><li>crafty_managed.txt is set aside</li></ul></div>
        {#each (st.plan?.notes || []).filter((n) => !n.startsWith('Crafty started it with plain')) as n (n)}<div class="tiny muted">{n}</div>{/each}
        {#if st.busy}
          <div class="warn small"><TriangleAlert size={13} /><span>{st.busy}</span></div>
        {:else}
          <div class="warn small"><TriangleAlert size={13} /><span>Crafty still lists it{st.crafty.auto_start ? ' and starts it with itself' : ''}. Remove it there (or never start it there) so both panels don't run it at once.</span></div>
        {/if}
      {/if}
      <div class="row end">
        <button class="btn" disabled={busy} onclick={() => (open = false)}>Cancel</button>
        <button class="btn pri" disabled={blocked || busy} onclick={take}>{busy ? 'Taking over…' : 'Take it over'}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .strip { margin: 0 18px 12px; display: flex; gap: 10px; align-items: center; border: 1px solid #a78bfa55; background: var(--violet-dim); border-radius: 8px; padding: 8px 12px; color: var(--violet); }
  .strip span { color: var(--subtle); }
  .strip b { color: var(--text); font-weight: 600; }
  .back { position: fixed; inset: 0; background: #0008; display: grid; place-items: center; z-index: 60; padding: 16px; }
  .box { width: min(560px, 100%); gap: 10px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0; font-size: 14px; }
  .kv { display: grid; grid-template-columns: 110px 1fr; gap: 6px 12px; }
  .kv > span { min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; display: block; }
  .chg { background: var(--base); border: 1px solid var(--line); border-radius: 8px; padding: 8px 12px; }
  .chg ul { margin: 4px 0 0 18px; }
  .warn { display: flex; gap: 8px; align-items: flex-start; color: var(--gold); background: var(--gold-dim); border: 1px solid #eab30855; border-radius: 8px; padding: 8px 10px; }
  .warn span { color: var(--text); }
  .end { justify-content: flex-end; }
  .icon { padding: 3px 5px; }
  @media (max-width: 700px) { .strip { flex-wrap: wrap; } .kv { grid-template-columns: 1fr; } }
</style>
