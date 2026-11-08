# Plan: "Changes in the files" for mods whose author wrote no changelog

Status: researched 2026-11-08, not built. Decided by the user: **opt in**, with a notice on the changelog view that offers to turn it on.

## What it is
When a mod version has no changelog from its author, Hopper can compare the old and new jar files and list what visibly changed. It is always labelled as scanned from the files, never shown as the author's words.

## Findings (real jars, 2026-11-08, scratch copies only)
| Mod and versions | Author's changelog | What the scan could say |
|---|---|---|
| Explorify 1.6.4 to 1.6.5 | empty | 473 of 473 files identical; version and supported pack format changed (88 to 512); optional Cristel Lib dependency dropped. "No content change" is a useful answer. |
| Spelunker's Charm II 5.0.6 to 5.1.0 | 6,269 characters | 8 new blocks (lead, tin, zinc, osmium boulders and a lush version of each), about 22 new config options with the author's own descriptions, 8 old boulder shapes replaced, 6 changed textures, Permafrost block removed. About 9 of 14 changelog points were visible as facts. Not seen: "candle helmet flame follows the head", most "fixed" lines. |
| Iron Furnaces 4.3.1 to 4.3.2 | empty | 6 furnace recipes "changed", but each only renamed `conditions` to `neoforge:conditions` (a format update). A naive scan would mislead. |
| Pipez 1.2.19 to 1.2.31 | one line (sable compatibility) | 3 Spanish translations added; 95 classes recompiled. The sable compatibility is not visible in the files. |

Tech mods with an empty or near-empty changelog in TNP LL8 (Modrinth-listed jars only: 777 of 1,139 found, 362 not checked): 159 tagged technology or energy, 28 with under 60 characters, 3 with none (Iron Furnaces, AE2 Overclocked, PipezRetriever).

## What the scan can honestly list
1. New and removed blocks and items (blockstates, item models, loot tables) with their names from the language file.
2. New and removed config options, with the description text the mod ships.
3. Languages added or removed.
4. Version, loader and Minecraft range, and added or removed dependencies from the mod metadata.
5. Data files (recipes, loot, tags, advancements) compared by meaning, not by bytes; a known format rename (`conditions` to `neoforge:conditions`) is shown as "format update, no gameplay change" or hidden.
6. When nothing readable changed: "Only the mod's code changed. The author hasn't written what." No "N classes changed" line.

Never state intent ("faster", "fixed"). Never decompile code to describe it.

## Opt-in and the notice
- Off by default. A setting "Show what changed in the files when a mod has no changelog" (Settings, Updates or Mods group; wording for players, one line explaining it downloads the old or new file once to compare).
- Where a changelog is empty and the setting is off, the card shows a short notice instead of "No changelog for this version": "The author didn't write a changelog. Hopper can compare the files to show what changed. [Turn on]". The button turns the setting on and scans that mod. A dismiss link hides the notice for that mod.
- With it on, the card shows the scan result under a clear label: "No changelog from the author. What changed in the files:" and "Scanned from the jar files by Hopper, not by the author."
- Same behaviour and wording in Hopper desktop and the Hopper Server web UI; the scan itself lives in `hopper-core` (the panel runs it for server mods and sends the result).

## Downloads, cache, cleanup
- Only runs when a changelog view is opened for a version with an empty changelog, never as part of "Update all".
- Reuse local files first: the installed jar is on disk, and a previous version may be in History; download only the missing one.
- A download goes to a temp folder (not the mods folder), is scanned, and is deleted straight away. The temp folder is cleared at start and exit.
- Cache the result, not the jar: a small file per (project, old file checksum, new file checksum). Stored in Hopper's own cache folder, a few kilobytes each, entries expire after about 90 days, safe to delete.
- No effect on History or backups: the scan changes none of the user's files.
- Network: Modrinth and CurseForge downloads as Hopper already does; respect the existing rate-limit handling.

## First slice (about 1.5 days)
1. `hopper-core`: jar compare producing a small structured result (blocks, items, config options, languages, metadata and dependency changes, meaningful data changes, "code only"), with fixture tests from the cases above (Explorify empty, Spelunker's content, Iron Furnaces format-only, Pipez code-only).
2. Setting and the notice on the shared changelog card (the one view used by the instance dialog and the mod peek).
3. Cache and temp-file handling with tests.
4. Web panel parity.

## Open points
- Pack-format number to Minecraft version name table (88 means nothing to players).
- Mods whose jars bundle other mods (jar-in-jar): scan only the top level at first.
- Mods not on Modrinth or CurseForge (sources from GitHub or links): compare only when both files are local.
- Language: the scan reads the English language file only.
