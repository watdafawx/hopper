# Publishing a mod so Hopper can install and update it

For agents and developers who publish their own mods outside CurseForge
and Modrinth. Hopper's "From GitHub or a link" (Add content ▾) and
"Follow a source" read your releases as described here. Players turn this on
in Settings › Mod sources. Hopper never runs anything from
a download: it only opens the jar as a zip to read its metadata.

## GitHub releases (best supported)

1. **One release per version, tagged with the version.** Hopper reads the
   version from the tag: it drops a leading `v`, a `loader/` prefix
   (`neoforge/v1.4.2`), an `mc1.21.1-` prefix, and `+build` metadata, then
   compares the numbers. `v1.4.2` is the safest form. Only strictly newer
   versions count as updates. Drafts are ignored. Updates look at the 30 most
   recent releases; a link to an older tag still installs that release.
2. **Never change a published release's files.** Hopper records each file's
   sha256 (GitHub lists it on the asset). If a release's file comes back with
   other contents under the same tag and name, Hopper flags it as "Changed at
   source" and won't install it on its own. Ship a new tag instead.
3. **Attach the jar as a release asset.** Source zips are ignored, and so is
   anything that isn't a `.jar`. Make one jar per loader and Minecraft
   version, and put both in the name, split by `-`, `+`, `_` or `.`:
   `modid-1.4.2+neoforge-1.21.1.jar`, `modid-1.4.2+fabric-1.21.1.jar`.
   Hopper recognises `neoforge`, `forge`, `fabric` and `quilt` (Quilt
   instances take Fabric jars), and Minecraft versions like `1.21.1`,
   `1.21`, `1.21.x` and the yearly `26.1`. Naming the loader only in the tag
   (`neoforge/v1.4.2`) also works.
4. **Skip extra jars by name.** Names ending in `-sources`, `-javadoc`,
   `-dev`, `-api`, `-slim` or `-dev-shadow` are never picked.
5. **Keep the same file-name shape across releases.** Updates must match
   the installed file's shape with every number folded away
   (`modid-#+neoforge-#.jar`), so one repo can ship several loaders without
   mixing them up. A `-alpha.2`, `-beta` or `-rc.1` suffix doesn't change the
   shape.
6. **Mark betas and alphas as GitHub pre-releases.** Players don't get them
   unless they tick "Include pre-releases".
7. **Write the release notes in the release body (Markdown).** Hopper shows
   them under "What changed" and in the update review.
8. **Keep the repo and mod id stable.** A mod follows `owner/repo` plus the
   file shape. If the repo moves, keep GitHub's redirect.

A jar whose name gives neither the loader nor the Minecraft version still
works. Hopper downloads it once to read its metadata (up to 6 files per
lookup, then cached), but that is slower, so name your files.

## Inside the jar

- Ship correct metadata: `META-INF/neoforge.mods.toml`, `META-INF/mods.toml`
  (Forge, and NeoForge before 20.5), `fabric.mod.json` or `quilt.mod.json`.
  After every download Hopper checks the loader and the `minecraft`
  dependency range (`versionRange = "[1.21.1,1.22)"`, or
  `"depends": { "minecraft": "~1.21.1" }`). A jar that doesn't fit isn't
  kept.
- Put a real `version` in it, not only a build placeholder like
  `${file.jarVersion}` left unexpanded. Hopper uses it to place a jar the
  player already has among your releases.
- Link your repo in the metadata: `issueTrackerURL` / `displayURL` (toml),
  or `contact.sources` / `homepage` / `issues` (fabric.mod.json), pointing
  at `https://github.com/owner/repo`. "Find sources for unknown mods" uses
  these links to offer your repo for jars players already have, without a
  reinstall: a jar that is byte for byte one of your assets is linked to
  that release, and any other jar to the release matching its version.

## Other sources

- **Maven:** give the artifact folder (`…/com/example/modid/`). Hopper reads
  its `maven-metadata.xml` (the newest 25 versions) and fetches
  `modid-<version>.jar`. Versions containing `alpha`, `beta`, `rc`, `pre` or
  `snapshot` count as pre-releases. Put the loader and Minecraft version in
  the version string if you publish several.
- **Jenkins:** give the job URL. Hopper takes the jars of the last
  successful build. The build number is the version, and the change
  messages are the notes.
- **Direct link:** a stable URL to a `.jar`. Hopper sees a new version when
  the server's `ETag` (or `Last-Modified`) changes, and takes the file name
  from `Content-Disposition` or the URL.
- Publishing on Modrinth or CurseForge too is fine. Hopper treats each
  source separately; players without Hopper find you there.

## Rate limits

Without a token, GitHub allows 60 API calls an hour. Hopper caches the
answers and only asks "changed since?", which doesn't count against the
limit. Mods from one repo share one call. You don't need to do anything for
this.

## GitHub Actions example

```yaml
name: Release
on:
  push:
    tags: ['v*']
permissions:
  contents: write
jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-java@v4
        with: { distribution: temurin, java-version: '21' }
      - run: ./gradlew build
      # Only the mod jars; -sources/-dev/-javadoc/-api jars stay out.
      - run: |
          jars=$(ls build/libs/*.jar | grep -Ev -- '-(sources|dev|javadoc|api)\.jar$')
          pre=""; case "$GITHUB_REF_NAME" in *-alpha*|*-beta*|*-rc*) pre="--prerelease";; esac
          gh release create "$GITHUB_REF_NAME" $jars --notes-file CHANGELOG-latest.md $pre
        env:
          GH_TOKEN: ${{ github.token }}
```
