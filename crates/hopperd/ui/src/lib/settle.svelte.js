// Keeps a busy notice from flashing, as hopper::settle does for Hopper
// desktop: the state must last `showAfter` ms before it shows, and once
// shown it stays `holdFor` ms. Call it inside a component: `const s =
// settled(() => busy)` then read `s.shown`.
export function settled(busy, showAfter = 700, holdFor = 1500) {
  let shown = $state(false);
  let shownAt = 0;
  $effect(() => {
    const on = !!busy();
    let timer;
    if (on) {
      timer = setTimeout(() => {
        shownAt = Date.now();
        shown = true;
      }, showAfter);
    } else if (shown) {
      timer = setTimeout(() => (shown = false), Math.max(0, holdFor - (Date.now() - shownAt)));
    }
    return () => clearTimeout(timer);
  });
  return {
    get shown() {
      return shown;
    },
  };
}
