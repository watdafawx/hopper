//! A mod from GitHub or a link, as a sync record carries it (design §2, the
//! `src:` keys).
//!
//! A CurseForge or Modrinth file is named by an id the friend's PC can look
//! up. A link has no such id: the other PC has to be told where the file
//! lives. So the `Value::File` id of such a mod is not a bare id but the
//! whole address, `link:<base64url json>`: the source (GitHub repo, Maven or
//! Jenkins URL, or the link itself), the file's name, its release, the
//! download URL when it is known without asking the source, and the sha256
//! the download is checked against. The id is a pure function of the
//! installed mod, so the PC that took the record in reads the same id back
//! from its own disk and shows "In sync". Pure.

use crate::sources::{Kind, ModSource};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

const PREFIX: &str = "link:";

/// Where one file of a link mod comes from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRef {
    pub kind: Kind,
    /// The source's address as `ModSource::url` has it.
    pub url: String,
    /// The release the file belongs to (tag, Maven version, build).
    pub seen: String,
    pub file_name: String,
    /// The file itself, when it can be told without asking the source.
    pub download: String,
    /// sha256 of the file, lower-case hex; empty when the source states none.
    pub sha256: String,
    pub pattern: String,
    pub publisher: String,
    /// CurseForge's class for packs and shaders; 0 for mods.
    pub class: u32,
}

impl LinkRef {
    /// What an installed mod's source says; `file_name` is the jar's name
    /// without ".disabled".
    pub fn of(src: &ModSource, file_name: &str) -> LinkRef {
        LinkRef {
            kind: src.kind,
            url: src.url.clone(),
            seen: src.seen.clone(),
            file_name: file_name.to_string(),
            download: download_of(src, file_name),
            sha256: src.sha256.clone(),
            pattern: src.pattern.clone(),
            publisher: src.publisher.clone(),
            class: src.class,
        }
    }

    /// The `Value::File` id.
    pub fn id(&self) -> String {
        let json = serde_json::to_vec(self).expect("a link is plain data");
        format!("{PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json))
    }

    /// Reads an id made by [`LinkRef::id`]; `None` for any other file id.
    pub fn parse(id: &str) -> Option<LinkRef> {
        let text = id.strip_prefix(PREFIX)?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// The source as the receiving PC keeps it on the mod.
    pub fn source(&self) -> ModSource {
        ModSource {
            kind: self.kind,
            url: self.url.clone(),
            pattern: self.pattern.clone(),
            prerelease: false,
            seen: self.seen.clone(),
            sha256: self.sha256.clone(),
            publisher: self.publisher.clone(),
            prev_project: 0,
            prev_mr_project: String::new(),
            class: self.class,
        }
    }

    /// The source's key, the one the mod's record key and the allow list use.
    pub fn source_key(&self) -> String {
        self.source().key()
    }
}

/// The address of the file in this release, for the sources where it follows
/// from what the mod keeps. A Jenkins artifact's path is not kept, so that
/// one is looked up in the source's listing instead.
fn download_of(src: &ModSource, file_name: &str) -> String {
    let (url, tag) = (src.url.trim_end_matches('/'), src.seen.as_str());
    match src.kind {
        Kind::Url => src.url.clone(),
        Kind::GitHub if !tag.is_empty() => format!("https://github.com/{url}/releases/download/{tag}/{file_name}"),
        Kind::Maven if !tag.is_empty() => format!("{url}/{tag}/{file_name}"),
        _ => String::new(),
    }
}

/// The record key of a mod that comes from a source: stable across its
/// releases, and the one the gate asks the player about ("src:github:o/r").
pub fn key_text(src: &ModSource) -> String {
    format!("src:{}", src.key())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github() -> ModSource {
        ModSource {
            kind: Kind::GitHub,
            url: "Owner/Repo".into(),
            pattern: "tweaks-#.jar".into(),
            seen: "v1.2".into(),
            sha256: "ab".repeat(32),
            publisher: "Owner/Repo".into(),
            ..ModSource::default()
        }
    }

    #[test]
    fn the_id_carries_the_address_and_the_checksum_and_reads_back() {
        let r = LinkRef::of(&github(), "tweaks-1.2.jar");
        assert_eq!(r.download, "https://github.com/Owner/Repo/releases/download/v1.2/tweaks-1.2.jar");
        let id = r.id();
        assert!(id.starts_with("link:"));
        let back = LinkRef::parse(&id).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.sha256, "ab".repeat(32));
        assert_eq!(back.source().key(), github().key());
    }

    #[test]
    fn a_receiver_reads_the_same_id_from_what_it_installed() {
        // What the receiving PC keeps on the mod is `LinkRef::source`; the id
        // read back from it must be the one that arrived.
        let sent = LinkRef::of(&github(), "tweaks-1.2.jar");
        let installed = sent.source();
        assert_eq!(LinkRef::of(&installed, "tweaks-1.2.jar").id(), sent.id());
    }

    #[test]
    fn other_file_ids_are_not_links() {
        assert!(LinkRef::parse("cf:11").is_none());
        assert!(LinkRef::parse("thing.jar").is_none());
        assert!(LinkRef::parse("link:!!!").is_none());
    }

    #[test]
    fn a_link_source_is_a_src_key_the_gate_asks_about() {
        let key = key_text(&github());
        assert_eq!(key, "src:github:owner/repo");
        let direct = ModSource { kind: Kind::Url, url: "https://x.test/a.jar".into(), ..ModSource::default() };
        assert_eq!(LinkRef::of(&direct, "a.jar").download, "https://x.test/a.jar");
        // A release nobody can name has no address until the source is asked.
        let jenkins = ModSource { kind: Kind::Jenkins, url: "https://ci.test/job/x".into(), ..ModSource::default() };
        assert_eq!(LinkRef::of(&jenkins, "a.jar").download, "");
    }
}
