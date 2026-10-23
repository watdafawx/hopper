export const toast = $state({ text: '', bad: false });

let timer;

export function say(text, bad = false) {
  toast.text = text;
  toast.bad = bad;
  clearTimeout(timer);
  timer = setTimeout(() => (toast.text = ''), bad ? 6000 : 3000);
}
