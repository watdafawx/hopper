# Visual changelog

The page at `index.html` (site: `/changelog/`) shows, per release, what changed from x to y, before/after screenshots, files touched, and how it was tested or not. Newest release first. `cards/` holds the source text; the page is written by hand from it (no build step).

## Add a release

1. Copy each session's `card.md` to `cards/<version>-<slug>.md`. Image links in it become `../img/<version>-<state>-<before|after>.jpg`.
2. Add screenshots to `img/`: JPEG, about 1200 px wide, quality 80, ideally under 200 KB. With Python/Pillow: `im.convert("RGB").resize(...).save(path, quality=80, optimize=True)`. Name: `<version>-<state>-<before|after>.jpg`.
3. In `index.html` copy the newest `<section class="rel">` and edit it: release heading and date, then one `<article class="cl">` per card (copy one). Put the new section above the older ones. The version rail at the top of the page, the "Jump to version" list and the `#<version>` links are built by the page script from the section's heading (first version number in it), date, first card title and its pills, so there is nothing else to edit. Keep the heading in the form `Hopper X.Y.Z · Hopper Server X.Y.Z`.
4. Pills: `ok` = seen on screen, `warn` = partly checked, `bad` = gap found. Plain words for players; file paths only in the `files` chips.
5. Open the page in a browser at desktop and phone width before committing.
