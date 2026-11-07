# Hopper Server web UI

Svelte 5 (runes) + Vite. `pnpm install`, then `pnpm build` (the server embeds `dist/`).

- `pnpm build` runs the loop check first, then Vite.
- `pnpm check` runs the loop check and the tests (`tests/`, plain `node --test`).

## Request loops

An effect that fetches and writes what it reads runs again when the answer
arrives, forever. In Settings > Sleep that was ~1,100 requests a minute until
the panel answered "Too many requests. Wait a minute." and the form kept
resetting itself. Three layers keep that from happening again:

1. **Loop check** (`scripts/check-effects.mjs`, run by `pnpm build` and
   `pnpm check`). It flags an `$effect` that
   - reads `$state` that it, or a same-file function it calls, assigns after an
     `await` or in a callback (`loop`);
   - calls `load*` / `fetch*` / `refresh*` / `reload*` with no `untrack()` and
     reads nothing itself (`key`): add a key line such as `sid;`;
   - reads `server.<x>` directly (`prop`): the live feed hands over a new `server`
     object every second, so read `const sid = $derived(server.id)` instead.

   A false positive gets `// loop-safe: <why it cannot loop>` on the `$effect`
   line or the line above. Say the real reason (a guard, a bound), not "ok".
   Fixtures for the check are in `tests/fixtures/`.
2. **Brake** (`src/lib/brake.js`, used by `src/lib/api.js`). Identical GETs in
   flight at the same moment share one request. More than 10 identical GETs in
   5 s are refused in the browser and the console says `request loop?: GET <url> x N in 5 s`;
   if the page keeps asking (30 in 10 s) one toast says so. A poll that is meant
   to be fast opts out with `api(path, { poll: true })`.
3. **Panel log** (`crates/hopperd/src/security.rs`): when one address reaches
   80% of the rate limit, one warning line a minute names the three busiest
   paths, so a loop shows in the log before it locks anyone out.

Pages that poll should skip the tick while `document.hidden`.
