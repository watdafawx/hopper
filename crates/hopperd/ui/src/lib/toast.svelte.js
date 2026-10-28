export const toast = $state({ text: '', bad: false });

let timer;

export function say(text, bad = false) {
  toast.text = String(text ?? '');
  toast.bad = bad;
  clearTimeout(timer);
  // Problems stay longer, and long lines get time to be read.
  const ms = (bad ? 6000 : 3000) + Math.max(0, toast.text.length - 80) * 30;
  timer = setTimeout(hush, Math.min(ms, 12000));
}

/** Puts the message away now. */
export function hush() {
  clearTimeout(timer);
  toast.text = '';
}
