/** Java arguments one per line, as user_jvm_args.txt has them: an
 * argument with spaces in quotes, so it stays one argument. */
export const flagsToText = (flags) => (flags || []).map((f) => (/\s/.test(f) ? `"${f}"` : f)).join('\n');

/** The arguments box without -Xms / -Xmx lines: the memory slider sets those. */
export const withoutMemory = (text) => text.split('\n').filter((l) => !/^\s*-Xm[sx]/i.test(l)).join('\n');
