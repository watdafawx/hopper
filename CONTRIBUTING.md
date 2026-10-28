# Contributing to Hopper

Forks and pull requests are welcome: fix a bug, add a server panel or a
launcher, bring Hopper desktop and Hopper Server closer together, or take it
somewhere new.

The full rules, for people and AI agents alike, are in
[AGENTS.md](AGENTS.md). The short version:

1. **One topic per pull request.** Open an issue first for bigger features.
2. **UI changes start as a mock-up**: two or three options in a small HTML
   file or screenshots, with the one you'd pick and why.
3. **Run it.** `cargo test -p hopper-core --lib`, `cargo test -p hopperd`
   (after `pnpm --dir crates/hopperd/ui build`), and check the change in the
   running app. Put screenshots in the PR.
4. **Never test on real modpacks, worlds or servers**: use a throwaway copy.
5. **Apply it everywhere**: the same fix in every place with the same
   pattern, and in both Hopper desktop and Hopper Server where it applies.
6. **No `cargo fmt` on the whole repo, no version bumps, no tags, no
   workflow changes, no secrets.**
7. **No code from projects whose license doesn't allow it**, the Crash
   Assistant mod in particular.
8. **Say if you used AI**, and which tool, in the PR.

Hopper is MIT licensed; by sending a pull request you agree your change is
too.
