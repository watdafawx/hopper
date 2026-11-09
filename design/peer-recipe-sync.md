# Peer recipe sync

Syncing two clients by copying jars is mostly unnecessary: Hopper already
has a compact recipe for an instance, the **share code** (`sharecode.rs`).
A code names the pack and its exact version, then only what differs from it,
including files from GitHub, Maven, Jenkins and links pinned by release and
sha256. Applying one downloads from the original sources.

This feature lets a paired peer *follow* another instance: the only thing
that crosses the peer link is the share code. Files come from CurseForge,
Modrinth, GitHub and the other sources on each PC, not from the sender.

## What is new

- **A recipe endpoint on the peer hub**: `GET /recipe?inst=<key>` behind the
  existing pairing token, returning `{ inst, name, code, left_out }`. The
  code is built with `sharecode::make`.
- **Apply in place**: `sharecode::apply` becomes public and can run on an
  existing instance. Removals go to backups through `install::remove`, not
  `std::fs::remove_file`.
- **Base check**: a code's base is compared with the local `Instance.pack`.
  - same pack file / same plain game + loader: apply the entries;
  - same pack, new file: update the pack with `modpack::update_any`, then
    apply;
  - different pack or a plain code against a pack instance: refuse with a
    clear message (install it as a new instance with the share code).
- **Follow record**: `settings.json` keeps, per local instance root, the
  peer, their instance key, the last applied code, the last code seen, and
  the files the recipe could not describe (`left_out`).
- **Peers tab**: "Follow their instance" in the sync panel; a status line
  for the current instance with Check, Update and Stop. Check is manual in
  this version; nothing polls in the background.

## What does not cross the wire

Jars, configs, resource packs, worlds. The only files that ever need a real
transfer are the ones `sharecode::make` reports as `left_out` (a local jar
with no source). The UI names them; sending those stays a separate,
explicit action.

## Wording and layout

The screen is now **Friends** (the tab and palette entry; the internal name
stays `Peers`). Its title is "Sync with a friend", with one sentence saying
what crosses the network: only the mod list, while files download from their
original sites. The empty state is a card ("No friend's PC here yet") with
one action, **Pair by address**, and the address, port, discovery error and
Windows firewall note live under **Advanced**. The manual file diff is under
**Files…**; the recipe is the primary action.

Mock: `mockups/peers-redesign.html` (Now, A, B, C; A is built).

## Deliberately out of scope (v1)

- Signatures and a public mailbox/relay. The recipe is fetched over the
  existing paired-peer connection, which is already authenticated by the
  pairing token. A mailbox would need a signature (peer key + revision)
  before it can be trusted.
- Automatic updates and notifications. Check happens when the player asks.
- Two-way sync. A follow is one-way: the follower matches the sender.
- Server panels. This is desktop peer sync; the panel already has its own
  push/config flow.

## Tests

- share-code recipe round trip on a plain instance (no network);
- base fit: same, pack update, different pack/plain;
- apply in place: off and remove entries, with the removed file landing in
  backups;
- `/recipe` over two loopback hubs, paired, with a plain instance.

The real network install paths stay covered by the existing ignored
`real_round_trip` tests; this change does not touch the download code.
