// The browser sessions this page holds for other panels: one token per
// panel address, in this page's localStorage (or sessionStorage with "Don't
// keep me signed in on this browser", gone when the tab closes). They never
// go to this panel. Storage can be off (a private window): then the page
// just asks to sign in again. Passwords are never kept.

const KEY = 'hopperd.panels.tokens';

function read(store) {
  try {
    return JSON.parse(store.getItem(KEY) || '{}') || {};
  } catch {
    return {};
  }
}

function write(store, v) {
  try {
    if (Object.keys(v).length) store.setItem(KEY, JSON.stringify(v));
    else store.removeItem(KEY);
  } catch {
    // storage off: this tab keeps it in memory only
  }
}

/** Tokens held only until this page reloads (storage off). */
const memory = {};

const stores = () => {
  const out = [];
  try {
    out.push(sessionStorage, localStorage);
  } catch {
    // no storage at all
  }
  return out;
};

export function tokenFor(url) {
  if (memory[url]) return memory[url];
  for (const s of stores()) {
    const t = read(s)[url];
    if (t) return t;
  }
  return null;
}

/** Keeps `token` for `url`; `keep: false` keeps it for this tab only. */
export function setToken(url, token, keep = true) {
  forgetToken(url);
  memory[url] = token;
  const s = stores();
  const store = keep ? s[1] : s[0];
  if (!store) return;
  const v = read(store);
  v[url] = token;
  write(store, v);
}

export function forgetToken(url) {
  delete memory[url];
  for (const s of stores()) {
    const v = read(s);
    if (url in v) {
      delete v[url];
      write(s, v);
    }
  }
}
