// One page at a time can ask before the panel leaves it (the file editor
// with unsaved changes). Shell checks this on every address change: links,
// tabs, the palette, back and forward.
//
// guard = { blocked(): bool, settle(): Promise<bool> }
//   blocked: true while leaving would drop something.
//   settle: asks the user; true when it's fine to go on.
export const leave = { guard: null };
