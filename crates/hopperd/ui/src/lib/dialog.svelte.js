// One small modal for questions: ask() for text, confirmIt() for yes/no,
// choose() for two answers (like "Turn off all 3" / "Only Create").

export const dialog = $state({ open: false, title: '', body: '', input: null, ok: 'OK', alt: '', danger: false });

let settle = null;

function show(opts) {
  if (settle) settle(null);
  Object.assign(dialog, { open: true, body: '', input: null, ok: 'OK', alt: '', danger: false }, opts);
  return new Promise((resolve) => (settle = resolve));
}

/** Asks for text; resolves to the text, or null if cancelled. */
export function ask(title, value = '', { body = '', ok = 'OK', placeholder = '', danger = false } = {}) {
  return show({ title, body, ok, danger, input: { value, placeholder } });
}

/** Asks yes or no; resolves to true or false. */
export async function confirmIt(title, { body = '', ok = 'OK', danger = false } = {}) {
  return (await show({ title, body, ok, danger })) === true;
}

/** Asks with two answers besides Cancel; resolves to 'ok', 'alt' or null. */
export async function choose(title, { body = '', ok = 'OK', alt = '', danger = false } = {}) {
  const r = await show({ title, body, ok, alt, danger });
  return r === true ? 'ok' : r === 'alt' ? 'alt' : null;
}

export function answer(value) {
  dialog.open = false;
  const s = settle;
  settle = null;
  s?.(value);
}
