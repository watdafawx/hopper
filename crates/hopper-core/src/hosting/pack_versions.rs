//! Which of a modpack's versions to offer when making a server from it:
//! only the ones with server files when the pack has any (the others would
//! be built from the client pack), a count of the client-only rest, and a
//! warning when the newest version has no server files yet. Desktop and
//! Hopper Server both use it, so they say the same thing.

use serde::Serialize;

/// The version list for a New server dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct VersionView {
    /// Indices (into the list given, newest first) to show.
    pub shown: Vec<usize>,
    /// Client-only versions left out of `shown`.
    pub hidden: usize,
    /// The line under the list for them ("12 more client-only versions").
    pub hidden_text: Option<String>,
    /// Set when the newest version has no server files but an older one does.
    pub warning: Option<String>,
    /// That newest version's index, to build it from the client pack.
    pub newest_client: Option<usize>,
}

/// `versions`: (name, has server files), newest first.
pub fn view(versions: &[(&str, bool)]) -> VersionView {
    let with: Vec<usize> = versions.iter().enumerate().filter(|(_, (_, s))| *s).map(|(i, _)| i).collect();
    if with.is_empty() {
        // No server files at all: every version is built from the client pack.
        return VersionView { shown: (0..versions.len()).collect(), ..Default::default() };
    }
    let hidden = versions.len() - with.len();
    let hidden_text = (hidden > 0).then(|| format!("{hidden} more client-only version{}", if hidden == 1 { "" } else { "s" }));
    let (warning, newest_client) = match versions.first() {
        Some((newest, false)) => {
            let server = versions[with[0]].0;
            (
                Some(format!(
                    "{newest} has no server files yet; the newest server files are {server}. Players on {newest} may not be able to join a {server} server."
                )),
                Some(0),
            )
        }
        _ => (None, None),
    };
    VersionView { shown: with, hidden, hidden_text, warning, newest_client }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_files_only_with_a_count_of_the_rest() {
        let v = view(&[("v53", true), ("v52.zip", false), ("v52", true), ("v51 · 1.21", false)]);
        assert_eq!(v.shown, vec![0, 2]);
        assert_eq!(v.hidden, 2);
        assert_eq!(v.hidden_text.as_deref(), Some("2 more client-only versions"));
        assert_eq!(v.warning, None);
    }

    #[test]
    fn warns_when_the_newest_has_no_server_files() {
        let v = view(&[("v54", false), ("v53", true)]);
        assert_eq!(v.shown, vec![1]);
        assert_eq!(v.newest_client, Some(0));
        assert_eq!(
            v.warning.unwrap(),
            "v54 has no server files yet; the newest server files are v53. Players on v54 may not be able to join a v53 server."
        );
        assert_eq!(v.hidden_text.as_deref(), Some("1 more client-only version"));
    }

    #[test]
    fn a_pack_without_server_files_shows_everything() {
        let v = view(&[("2.0", false), ("1.0", false)]);
        assert_eq!(v.shown, vec![0, 1]);
        assert_eq!((v.hidden, v.warning, v.hidden_text), (0, None, None));
    }
}
