// "Changes in the files" (design/changelog-from-files.md), the panel's side:
// the setting's current value once something changed it, and which mods the
// owner said "Not now" for (this browser only; Hopper desktop keeps it in its
// settings). The words are hopper-core's `changelog_files` constants.
export const NOTICE = "The author didn't write a changelog. Hopper can compare the files to show what changed.";
export const LABEL = 'No changelog from the author. What changed in the files:';
export const FOOTER = 'Scanned from the jar files by Hopper, not by the author.';
export const HELP = 'Show what changed in the files when a mod has no changelog. Hopper compares the old and new file; it may download one of them once.';
export const NONE = 'No changelog for this version.';

/** `on` is null until the setting is changed in this page; then it wins over what the server said. */
export const fc = $state({ on: null, hidden: {} });

const KEY = (project) => `hopperd.files-changes.hidden.${project}`;

export function isHidden(project) {
  if (fc.hidden[project] !== undefined) return fc.hidden[project];
  try {
    return localStorage.getItem(KEY(project)) === '1';
  } catch {
    return false;
  }
}

export function hide(project) {
  fc.hidden[project] = true;
  try {
    localStorage.setItem(KEY(project), '1');
  } catch {
    /* private window: hidden until the page reloads */
  }
}
