// Loop check for `$effect`s: run by `pnpm build` and `pnpm check`.
//
// Why: an effect that reads `$state` which it (or a function it calls)
// assigns after an `await` runs again when the answer arrives, forever. In
// Settings > Sleep that was one request per round trip until the panel's
// rate limit answered "Too many requests", and it kept resetting the form.
//
// What it flags, per `$effect` / `$effect.pre`:
//   loop   The effect reads a `$state` variable (before its first `await`,
//          outside `untrack`) that it, or a same-file function it calls,
//          assigns later (after an `await`, in a `.then`, a timer, ...).
//   key    The effect calls load* / fetch* / refresh* / reload* with no
//          `untrack` and reads nothing itself, so what it depends on is
//          whatever the function happens to read. Add a key line (`sid;`).
//   prop   The effect reads `server.<x>` directly. The live feed hands
//          over a new `server` object every second, so that reruns every
//          second. Take the id (`const sid = $derived(server.id)`) and read that.
//
// False positives are fine: put `// loop-safe: <the reason>` on the line of
// the `$effect(` or on the line above it. Say why it can't loop.
//
// Usage: node scripts/check-effects.mjs [dir-or-file ...]   (default: src)
// Exit code 1 when something is flagged.

import fs from 'node:fs';
import path from 'node:path';
import { parse } from 'svelte/compiler';

const FETCHY = /^(load|fetch|refresh|reload)/i;
/** Props the live feed replaces every second. */
const LIVE_PROPS = new Set(['server']);
/** Calls whose callbacks run later, not while the effect runs. */
const LATER = new Set(['setTimeout', 'setInterval', 'queueMicrotask', 'requestAnimationFrame', 'then', 'catch', 'finally', 'addEventListener', 'onChange']);

function walkFiles(p) {
  const st = fs.statSync(p);
  if (st.isFile()) return [p];
  return fs.readdirSync(p).flatMap((n) => walkFiles(path.join(p, n)));
}

function children(node) {
  const out = [];
  for (const k of Object.keys(node)) {
    if (k === 'loc' || k === 'start' || k === 'end' || k === 'type') continue;
    const v = node[k];
    if (Array.isArray(v)) for (const x of v) if (x && typeof x.type === 'string') out.push(x);
    if (v && typeof v.type === 'string') out.push(v);
  }
  return out;
}

const isFn = (n) => n.type === 'FunctionDeclaration' || n.type === 'FunctionExpression' || n.type === 'ArrowFunctionExpression';
const isRune = (n, ...names) =>
  n?.type === 'CallExpression' && names.includes(n.callee.type === 'MemberExpression' ? `${n.callee.object.name}.${n.callee.property.name}` : n.callee.name);

function rootName(n) {
  while (n && n.type === 'MemberExpression') n = n.object;
  return n?.type === 'Identifier' ? n.name : null;
}

export function checkSource(src, file = 'input.svelte') {
  const isSvelte = file.endsWith('.svelte');
  // `.js` modules go through the same parser inside a script tag.
  const text = isSvelte ? src : `<script>${src}</script>`;
  const offset = isSvelte ? 0 : '<script>'.length;
  let ast;
  try {
    ast = parse(text, { modern: true });
  } catch (e) {
    return [{ file, line: 1, kind: 'parse', msg: `could not parse: ${e.message}` }];
  }
  const roots = [ast.instance?.content, ast.module?.content].filter(Boolean);
  const lineOf = (n) => text.slice(0, n.start).split('\n').length;
  const lines = text.split('\n');
  const loopSafe = (n) => {
    const l = lineOf(n);
    return /loop-safe:\s*\S/.test(lines[l - 1] || '') || /loop-safe:\s*\S/.test(lines[l - 2] || '');
  };

  // Reactive variables and local functions.
  const state = new Set();
  /** Everything reactive that can be read: state, props and $derived. */
  const reactive = new Set();
  const fns = new Map();
  const propNames = new Set();
  for (const root of roots) {
    for (const st of root.body) {
      const decl = st.type === 'ExportNamedDeclaration' && st.declaration ? st.declaration : st;
      if (decl.type === 'FunctionDeclaration') fns.set(decl.id.name, decl);
      if (decl.type !== 'VariableDeclaration') continue;
      for (const d of decl.declarations) {
        if (d.id.type === 'Identifier' && d.init && isFn(d.init)) fns.set(d.id.name, d.init);
        if (isRune(d.init, '$state', '$state.raw', '$bindable')) {
          if (d.id.type === 'Identifier') { state.add(d.id.name); reactive.add(d.id.name); }
        }
        if (isRune(d.init, '$derived', '$derived.by') && d.id.type === 'Identifier') reactive.add(d.id.name);
        if (isRune(d.init, '$props') && d.id.type === 'ObjectPattern') {
          for (const p of d.id.properties) {
            const name = p.value?.type === 'AssignmentPattern' ? p.value.left.name : p.value?.name;
            if (name) { state.add(name); reactive.add(name); propNames.add(name); }
          }
        }
      }
    }
  }

  const findings = [];

  function analyse(effect) {
    const reads = new Map(); // name -> first node, read while tracked
    const writes = new Set(); // names assigned later
    let calledFetchy = null;
    let hasUntrack = false;
    const bodyReads = new Set(); // reactive things the effect's own lines read
    let depth = 0;
    const seen = new Set();

    // `late`: code that runs after the effect finished (callbacks, after an await).
    const ctx = { late: false };

    function visit(n, late) {
      if (!n || typeof n.type !== 'string') return late;
      switch (n.type) {
        case 'CallExpression': {
          const cal = n.callee;
          const name = cal.type === 'Identifier' ? cal.name : cal.type === 'MemberExpression' ? cal.property.name : '';
          if (name === 'untrack') {
            hasUntrack = true;
            return late;
          }
          if (!late && cal.type === 'Identifier' && FETCHY.test(name) && !calledFetchy) calledFetchy = name;
          const laterCb = LATER.has(name);
          late = visit(cal, late);
          for (const a of n.arguments) {
            if (isFn(a)) visitFnBody(a, laterCb ? true : late, true);
            else late = visit(a, late);
          }
          if (cal.type === 'Identifier' && fns.has(cal.name) && !seen.has(cal.name) && !laterCb) {
            seen.add(cal.name);
            depth++;
            visitFnBody(fns.get(cal.name), late, false);
            depth--;
          }
          return late;
        }
        case 'AwaitExpression':
          visit(n.argument, late);
          return true;
        case 'AssignmentExpression': {
          late = visit(n.right, late);
          const r = rootName(n.left);
          if (r && state.has(r) && late) writes.add(r);
          if (n.left.type === 'MemberExpression') late = visit(n.left, late);
          else if (n.operator !== '=' && r && state.has(r) && !late && !reads.has(r)) reads.set(r, n);
          return late;
        }
        case 'UpdateExpression': {
          const r = rootName(n.argument);
          if (r && state.has(r) && late) writes.add(r);
          return late;
        }
        case 'Identifier':
          if (reactive.has(n.name) && !late && depth === 0) bodyReads.add(n.name);
          if (state.has(n.name) && !late && !reads.has(n.name)) reads.set(n.name, n);
          return late;
        case 'MemberExpression': {
          const r = rootName(n);
          if (r && LIVE_PROPS.has(r) && propNames.has(r) && !late && n.object.type === 'Identifier' && !(n.property.name === 'id' && false)) {
            liveReads.add(`${r}.${n.computed ? '[]' : n.property.name}`);
          }
          late = visit(n.object, late);
          if (n.computed) late = visit(n.property, late);
          return late;
        }
        case 'Property':
          if (n.computed) late = visit(n.key, late);
          return visit(n.value, late);
        default:
          if (isFn(n)) {
            visitFnBody(n, true, true);
            return late;
          }
          for (const c of children(n)) late = visit(c, late);
          return late;
      }
    }

    // Body of a function. `inherited`: already late when it starts.
    // A nested closure's locals shadow nothing we track, so names are matched flat.
    function visitFnBody(fn, inherited, closure) {
      let late = inherited;
      // A called (not nested) function that awaits only goes late after its first await.
      late = visit(fn.body, late);
      return late;
    }

    const liveReads = new Set();
    const cb = effect.arguments[0];
    if (!cb || !isFn(cb)) return;
    visit(cb.body, false);

    const loc = lineOf(effect);
    if (loopSafe(effect)) return;
    const loop = [...reads.keys()].filter((r) => writes.has(r));
    if (loop.length) {
      findings.push({ file, line: loc, kind: 'loop', msg: `$effect reads $state ${loop.map((x) => '`' + x + '`').join(', ')} and assigns it after an await/callback, so it can run again forever. Wrap the reload in untrack() or key on a derived id; // loop-safe: <reason> if it can't loop.` });
    }
    if (calledFetchy && !hasUntrack && bodyReads.size === 0) {
      findings.push({ file, line: loc, kind: 'key', msg: `$effect calls ${calledFetchy}() with no untrack() and reads nothing itself, so it depends on whatever it happens to read. Add a key line (\`sid;\`) or untrack().` });
    }
    if (liveReads.size) {
      findings.push({ file, line: loc, kind: 'prop', msg: `$effect reads ${[...liveReads].join(', ')}: the live feed replaces \`server\` every second, so this reruns every second. Read a derived id (\`const sid = $derived(server.id)\`) instead.` });
    }
  }

  function find(n) {
    if (!n || typeof n.type !== 'string') return;
    if (n.type === 'CallExpression' && isRune(n, '$effect', '$effect.pre')) analyse(n);
    for (const c of children(n)) find(c);
  }
  for (const root of roots) find(root);
  // Effects inside the template (rare): `{@const}` etc. are not scanned.

  return findings.map((f) => ({ ...f, line: f.line }));
}

export function checkFiles(paths) {
  const out = [];
  for (const p of paths.flatMap(walkFiles)) {
    if (!/\.(svelte|svelte\.js)$/.test(p) && !/\.js$/.test(p)) continue;
    const src = fs.readFileSync(p, 'utf8');
    if (!src.includes('$effect')) continue;
    out.push(...checkSource(src, p));
  }
  return out;
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'))) {
  const targets = process.argv.slice(2);
  const found = checkFiles(targets.length ? targets : ['src']);
  for (const f of found) console.error(`${f.file}:${f.line}: [${f.kind}] ${f.msg}`);
  if (found.length) {
    console.error(`\ncheck-effects: ${found.length} finding(s). Fix them, or add "// loop-safe: <reason>" above the $effect.`);
    process.exit(1);
  }
  console.log('check-effects: ok');
}
