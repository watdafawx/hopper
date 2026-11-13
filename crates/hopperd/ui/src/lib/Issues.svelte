<script>
  // A project's GitHub issues, as desktop's Issues tab: a search box, Open /
  // Closed / All, and 20 issues at a time, each with its state, title,
  // labels, number, who opened it and when, and its comment count;
  // clicking one opens it on GitHub. The panel asks GitHub (the browser
  // never does) and keeps each answer for the session; GitHub only answers
  // a few searches a minute without a token, so typing searches once it
  // pauses, and a refusal says so.
  import { untrack } from 'svelte';
  import { Search, X, CircleDot, CircleCheck, MessageSquare, SearchX, TriangleAlert, Clock, ExternalLink } from '@lucide/svelte';
  import { api } from './api.js';
  import { ago, date } from './browse.js';
  import { rightClear } from './rightclear.js';

  // `project`: the project's number (CurseForge, Modrinth or a GitHub mod);
  // `repo`: its GitHub "owner/repo", for the link before anything loads.
  let { project, repo = '' } = $props();

  const FILTERS = [['open', 'Open'], ['closed', 'Closed'], ['all', 'All']];
  const BUSY = 'GitHub is busy, try again in a minute.';
  const FAILED = "Issues couldn't be loaded.";

  let text = $state('');
  let query = $state('');
  let filter = $state('open');
  // "project|filter|query" -> { list, next, more, total, busy, error, site }
  const seen = new Map();
  let st = $state(fresh());
  let timer;

  function fresh() {
    return { list: [], next: 1, more: false, total: null, busy: true, error: '', site: '' };
  }
  const keyOf = (pid, f, q) => `${pid}|${f}|${q.trim().toLowerCase()}`;
  const iso = (s) => (s ? new Date(s * 1000).toISOString() : '');
  const site = $derived(st.site || (repo ? `https://github.com/${repo}/issues` : ''));

  function meta(i) {
    const by = i.author ? ` by ${i.author}` : '';
    // "by" is who opened it, so a closed one names them before when it closed.
    return i.open || !i.closed ? `#${i.number} · opened ${ago(iso(i.created))}${by}` : `#${i.number}${by} · closed ${ago(iso(i.closed))}`;
  }

  async function load(pid, f, q, page) {
    const key = keyOf(pid, f, q);
    st.busy = true;
    st.error = '';
    try {
      const r = await api(`/browse/issues?project=${encodeURIComponent(pid)}&state=${f}&q=${encodeURIComponent(q)}&page=${page}`);
      if (keyOf(project, filter, query) !== key) return;
      st.site = r.site || st.site;
      if (r.error) st.error = r.error === 'busy' ? BUSY : FAILED;
      else {
        // A later page can repeat the end of the one before when issues came in.
        const have = new Set(st.list.map((i) => i.number));
        st.list = [...st.list, ...r.issues.filter((i) => !have.has(i.number))];
        st.next = page + 1;
        st.more = r.more;
        st.total = r.total ?? null;
      }
    } catch {
      if (keyOf(project, filter, query) !== key) return;
      st.error = FAILED;
    }
    st.busy = false;
    seen.set(key, $state.snapshot(st));
    // A page of only pull requests shows nothing: look a little further.
    if (!st.list.length && !st.error && st.more && st.next <= 3) load(pid, f, q, st.next);
  }

  function show(pid, f, q) {
    const kept = seen.get(keyOf(pid, f, q));
    if (kept) st = kept;
    else {
      st = fresh();
      load(pid, f, q, 1);
    }
  }

  // Another project starts over (unsearched, open ones).
  $effect(() => {
    const pid = project;
    untrack(() => {
      clearTimeout(timer);
      text = '';
      query = '';
      filter = 'open';
      show(pid, filter, query);
    });
  });

  function search(now = false) {
    clearTimeout(timer);
    const go = () => {
      const q = text.trim();
      if (q === query) return;
      query = q;
      show(project, filter, query);
    };
    if (now) go();
    else timer = setTimeout(go, 500);
  }

  function pick(f) {
    if (f === filter) return;
    filter = f;
    show(project, filter, query);
  }

  // A label in its own colour, mixed toward the page's text so it reads on
  // light and dark themes alike, on a faint wash of it.
  const tint = (c) => (c ? `--lc:#${c}` : '');
</script>

<div class="issues">
  <div class="input">
    <Search size={14} />
    <input use:rightClear bind:value={text} oninput={() => search()} onkeydown={(e) => e.key === 'Enter' && search(true)} placeholder="Search issues" aria-label="Search issues" />
    {#if text}<button class="clr" aria-label="Clear search" onclick={() => { text = ''; search(true); }}><X size={13} /></button>{/if}
  </div>
  <div class="row filters">
    <div class="seg" role="group" aria-label="Which issues">
      {#each FILTERS as [f, label] (f)}<button aria-pressed={filter === f} class:on={filter === f} onclick={() => pick(f)}>{label}</button>{/each}
    </div>
    <span class="sp"></span>
    {#if st.list.length && st.total != null}<span class="tiny muted">{st.total === 1 ? '1 found' : `${st.total.toLocaleString()} found`}</span>{/if}
  </div>

  {#if st.list.length}
    <div class="list">
      {#each st.list as i (i.number)}
        <a class="iss row-hover" href={i.url} target="_blank" rel="noreferrer" title={i.url}>
          <span class="st" class:closed={!i.open}>{#if i.open}<CircleDot size={16} />{:else}<CircleCheck size={16} />{/if}</span>
          <span class="col grow">
            <b class="small ttl">{i.title}</b>
            {#if i.labels.length}<span class="row labels">{#each i.labels as l (l.name)}<span class="chip ilabel" class:plain={!l.color} style={tint(l.color)}>{l.name}</span>{/each}</span>{/if}
            <span class="row tiny muted mt">
              <span class="ell" title={date(iso(i.open || !i.closed ? i.created : i.closed))}>{meta(i)}</span>
              {#if i.comments}<span class="cc"><MessageSquare size={12} />{i.comments}</span>{/if}
            </span>
          </span>
        </a>
      {/each}
    </div>
  {:else if st.busy}
    {#each Array(5) as _, i (i)}<div class="skel"><i></i></div>{/each}
  {:else if st.error === BUSY}
    <div class="empty-state"><span class="es-ic"><Clock size={18} /></span><b>{BUSY}</b><p>GitHub only answers a few searches a minute. They're still on its page.</p></div>
  {:else if st.error}
    <div class="empty-state"><span class="es-ic"><TriangleAlert size={18} /></span><b>{st.error}</b><p>GitHub didn't answer. They're still on its page.</p></div>
  {:else}
    <div class="empty-state"><span class="es-ic"><SearchX size={18} /></span><b>No issues match</b><p>{query ? 'Try other words, or All.' : 'Nothing here for this filter.'}</p></div>
  {/if}
  {#if st.list.length && st.error}<div class="small err">{st.error}</div>{/if}
  <div class="row foot">
    {#if st.more && st.list.length}<button class="btn sm" disabled={st.busy} onclick={() => load(project, filter, query, st.next)}>{st.busy ? 'Loading…' : 'Show more'}</button>{/if}
    <span class="sp"></span>
    {#if site}<a class="btn sm ghost" href={site} target="_blank" rel="noreferrer"><ExternalLink size={13} />Open issues on GitHub</a>{/if}
  </div>
</div>

<style>
  .issues { display: flex; flex-direction: column; gap: 10px; max-width: 860px; }
  .input input { flex: 1; min-width: 0; background: none; border: 0; outline: none; color: var(--text); font: inherit; }
  .clr { display: grid; place-items: center; color: var(--muted); }
  .clr:hover { color: var(--text); }
  .filters { align-items: center; gap: 8px; }
  .seg { display: inline-flex; border: 1px solid var(--line2); border-radius: var(--radius); overflow: hidden; }
  .seg button { padding: 3px 10px; font-size: 12px; color: var(--subtle); }
  .seg button + button { border-left: 1px solid var(--line2); }
  .seg button:hover:not(.on) { background: var(--hover); color: var(--text); }
  .seg button.on { background: var(--sel); color: var(--text); font-weight: 600; }
  .seg button:focus-visible { outline: 2px solid var(--acc); outline-offset: -2px; }
  .list { display: flex; flex-direction: column; }
  .iss { display: flex; gap: 10px; padding: 10px 8px; border-radius: var(--radius); align-items: flex-start; color: var(--text); text-decoration: none; }
  .iss + .iss { border-top: 1px solid var(--line); border-radius: 0; }
  .iss:focus-visible { outline: 2px solid var(--acc); outline-offset: -2px; }
  .st { flex: none; display: grid; padding-top: 1px; color: var(--green); }
  /* GitHub's purple for closed, nearer the text colour on light themes so it reads. */
  .st.closed { color: color-mix(in srgb, var(--violet) 75%, var(--text)); }
  .grow { flex: 1; min-width: 0; gap: 4px; }
  .ttl { font-weight: 600; line-height: 1.4; overflow-wrap: anywhere; }
  .labels { flex-wrap: wrap; gap: 4px; }
  .ilabel { height: 18px; font-size: 11px; padding: 0 6px 1px; color: color-mix(in srgb, var(--lc) 40%, var(--text)); background: color-mix(in srgb, var(--lc) 16%, transparent); }
  .ilabel.plain { color: var(--subtle); background: var(--hover); }
  .mt { gap: 12px; align-items: center; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .cc { display: inline-flex; align-items: center; gap: 4px; flex: none; }
  .foot { gap: 8px; margin-top: 4px; align-items: center; }
  .sp { flex: 1; }
  .err { color: var(--red); }
</style>
