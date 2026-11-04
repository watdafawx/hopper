<script>
  // Jars dropped anywhere on the page go into the server's mods, as on an
  // instance in Hopper desktop. Several files go one after the other, each
  // with its own progress; afterwards the new jars are looked up.
  import { FileUp, Check, X, TriangleAlert } from '@lucide/svelte';
  import { choose } from './lib/dialog.svelte.js';
  import { say } from './lib/toast.svelte.js';

  let { server, onAdded } = $props();

  let over = $state(false);
  // Rows of the progress card: { id, name, state: 'wait'|'up'|'done'|'fail'|'skip', pct, note }.
  let rows = $state([]);
  let depth = 0;
  let next = 1;
  let working = false;
  const queue = [];

  const hasFiles = (e) => [...(e.dataTransfer?.types || [])].includes('Files');
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));

  function enter(e) {
    if (!hasFiles(e)) return;
    e.preventDefault();
    depth++;
    over = true;
  }
  function overIt(e) {
    if (!hasFiles(e)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'copy';
  }
  function leave(e) {
    if (!hasFiles(e)) return;
    depth = Math.max(0, depth - 1);
    if (!depth) over = false;
  }
  function drop(e) {
    if (!hasFiles(e)) return;
    e.preventDefault();
    depth = 0;
    over = false;
    add([...e.dataTransfer.files]);
  }

  $effect(() => {
    window.addEventListener('dragenter', enter);
    window.addEventListener('dragover', overIt);
    window.addEventListener('dragleave', leave);
    window.addEventListener('drop', drop);
    return () => {
      window.removeEventListener('dragenter', enter);
      window.removeEventListener('dragover', overIt);
      window.removeEventListener('dragleave', leave);
      window.removeEventListener('drop', drop);
    };
  });

  function add(files) {
    const jars = files.filter((f) => /\.jar$/i.test(f.name));
    const other = files.length - jars.length;
    if (other) say(`${other} file${other === 1 ? ' was' : 's were'} left out: only .jar files can be dropped here.`, true);
    for (const f of jars) {
      const row = { id: next++, name: f.name, state: 'wait', pct: 0, note: '' };
      rows.push(row);
      queue.push({ f, row: rows[rows.length - 1], sid: server.id });
    }
    if (!working) run();
  }

  function send(sid, f, conflict, row) {
    return new Promise((resolve) => {
      const x = new XMLHttpRequest();
      x.open('POST', `/api/v1/servers/${encodeURIComponent(sid)}/mods/upload?name=${encodeURIComponent(f.name)}&conflict=${conflict}`);
      x.setRequestHeader('X-Hopperd', '1');
      x.setRequestHeader('Content-Type', 'application/octet-stream');
      x.upload.onprogress = (e) => (row.pct = e.lengthComputable ? Math.round((e.loaded / e.total) * 100) : 0);
      x.onload = () => {
        let body = null;
        try { body = JSON.parse(x.responseText); } catch { /* none */ }
        resolve({ status: x.status, body });
      };
      x.onerror = () => resolve({ status: 0, body: { error: 'Connection lost' } });
      x.send(f);
    });
  }

  async function run() {
    working = true;
    let added = 0;
    let replacedAny = false;
    while (queue.length) {
      const { f, row, sid } = queue.shift();
      row.state = 'up';
      let r = await send(sid, f, 'ask', row);
      if (r.status === 409 && r.body?.conflict) {
        // The same mod is on the server already (matched by the id inside
        // the jar, so a new version with another file name counts).
        const c = r.body.conflict;
        const v = (x) => (x ? ` ${x}` : '');
        const same = c.old_version && c.old_version === c.new_version;
        const a = await choose(`Replace ${c.mod}${v(c.old_version)} with${v(c.new_version) || ' this file'}?`, {
          body: `${c.old_file} is on the server${same ? ' with the same version' : ''}. The old jar goes to Backups, where it can be put back. Cancel skips ${f.name}.`,
          ok: 'Replace',
          alt: c.same_name ? '' : 'Keep both',
        });
        if (!a) {
          row.state = 'skip';
          row.note = 'Skipped';
          continue;
        }
        r = await send(sid, f, a === 'ok' ? 'replace' : 'both', row);
      }
      if (r.status === 200) {
        row.state = 'done';
        row.pct = 100;
        row.note = r.body?.replaced ? 'Replaced' : 'Added';
        added++;
        replacedAny ||= !!r.body?.replaced;
      } else {
        row.state = 'fail';
        row.note = r.body?.error || `Failed (${r.status})`;
      }
    }
    working = false;
    if (added) {
      say(`${added} jar${added === 1 ? '' : 's'} added to ${server.name}'s mods.${alive ? ' Restart the server to load them.' : ''}`);
      onAdded?.();
    }
    // The card stays while something failed, so the reason can be read.
    if (!rows.some((x) => x.state === 'fail')) setTimeout(() => !working && (rows = []), 4000);
  }
</script>

{#if over}
  <div class="drop-veil" role="presentation">
    <div class="drop-box">
      <FileUp size={30} />
      <div class="t">Drop .jar files to add them to {server.name}'s mods</div>
      {#if alive}<div class="s"><TriangleAlert size={13} />The server is running: restart it to load them.</div>{/if}
    </div>
  </div>
{/if}

{#if rows.length}
  <div class="drop-card" role="status">
    <div class="h"><span>Adding mods</span><button class="btn sm ghost icon" title="Close" disabled={working} onclick={() => (rows = [])}><X size={13} /></button></div>
    {#each rows as r (r.id)}
      <div class="r" class:bad={r.state === 'fail'}>
        <span class="n" title={r.name}>{r.name}</span>
        {#if r.state === 'up'}<span class="muted">{r.pct}%</span>
        {:else if r.state === 'done'}<span class="ok"><Check size={13} />{r.note}</span>
        {:else if r.state === 'wait'}<span class="muted">Waiting</span>
        {:else}<span class:warn={r.state === 'fail'} class:muted={r.state === 'skip'}>{r.note}</span>{/if}
        {#if r.state === 'up'}<div class="bar"><i style:width="{r.pct}%"></i></div>{/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .drop-veil { position: fixed; inset: 0; z-index: 150; background: color-mix(in srgb, var(--bg) 70%, transparent); display: flex; align-items: center; justify-content: center; pointer-events: none; }
  .drop-box { display: flex; flex-direction: column; align-items: center; gap: 8px; padding: 36px 56px; border: 2px dashed var(--acc); border-radius: var(--radius-lg); background: var(--popover); color: var(--acc); max-width: min(520px, calc(100vw - 32px)); text-align: center; }
  .drop-box .t { font-size: 15px; font-weight: 600; color: var(--text); }
  .drop-box .s { display: flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--subtle); }
  .drop-card { position: fixed; right: 16px; bottom: 16px; z-index: 90; width: min(340px, calc(100vw - 32px)); max-height: 50vh; overflow: auto; background: var(--popover); border: 1px solid var(--line); border-radius: var(--radius-lg); padding: 8px 12px; box-shadow: 0 16px 40px #000a; font-size: 12.5px; }
  .h { display: flex; align-items: center; justify-content: space-between; font-weight: 600; margin-bottom: 4px; }
  .r { display: grid; grid-template-columns: 1fr auto; gap: 2px 10px; padding: 4px 0; }
  .n { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ok { display: inline-flex; align-items: center; gap: 4px; color: var(--ok, #6cc070); }
  .r.bad { grid-template-columns: 1fr; }
  .bar { grid-column: 1 / -1; height: 3px; background: var(--raise); border-radius: 2px; overflow: hidden; }
  .bar i { display: block; height: 100%; background: var(--acc); transition: width .15s; }
</style>
