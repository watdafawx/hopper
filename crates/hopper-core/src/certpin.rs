//! Trust on first use for servers that make their own TLS certificate
//! (Crafty, MCSS and Pterodactyl panels, FTPS server files): the user trusts
//! one certificate by its SHA-256, and from then on only that one connects.
//! This holds what a run has seen of each one's certificate, shared by every
//! kind of connection so the rules (and the words) are the same everywhere.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};

/// What a certificate protects, for the words that name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Subject {
    /// A Crafty / MCSS / Pterodactyl panel (its API key).
    Panel,
    /// A server reached over FTPS (its password).
    Files,
}

impl Subject {
    /// "panel": as in "the panel's certificate".
    pub fn what(self) -> &'static str {
        match self {
            Subject::Panel => "panel",
            Subject::Files => "server",
        }
    }

    /// What is held back until the certificate is trusted.
    pub fn secret(self) -> &'static str {
        match self {
            Subject::Panel => "API key",
            Subject::Files => "password",
        }
    }
}

/// What Hopper saw of one certificate this run.
#[derive(Clone, Debug, Default)]
struct CertSeen {
    /// Pinned on this run (saved before pinning), not yet in the settings;
    /// it holds from the moment it's seen.
    learned: Option<String>,
    /// Saved before pinning, and it answered with a certificate an
    /// authority signs: it needs no pin, and shouldn't learn one later.
    vouched: bool,
    /// `learned` or `vouched` was handed to [`take_learned`].
    told: bool,
    /// The certificate changed: (was, now).
    changed: Option<(String, String)>,
}

type Certs = HashMap<(Subject, String), CertSeen>;
static CERTS: LazyLock<Mutex<Certs>> = LazyLock::new(Default::default);

fn certs() -> MutexGuard<'static, Certs> {
    CERTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether `id` may go on with certificate `fp`, given its saved pin and
/// whether the user is still to trust one. `vouched`: an authority signs it,
/// so it passes anyway.
pub(crate) fn check(what: Subject, id: &str, pin: &str, ask: bool, fp: &str, vouched: bool) -> Result<(), String> {
    let mut all = certs();
    let c = all.entry((what, id.to_string())).or_default();
    if vouched {
        // Learning stops here: a self-made certificate showing up later
        // has to be trusted by the user, never picked up quietly.
        if pin.is_empty() && !ask && c.learned.is_none() {
            c.vouched = true;
        }
        c.changed = None;
        return Ok(());
    }
    let want = if pin.is_empty() { c.learned.clone() } else { Some(pin.to_string()) };
    match want {
        Some(w) if crate::tls::same_fingerprint(&w, fp) => {
            c.changed = None;
            Ok(())
        }
        Some(w) => {
            c.changed = Some((w.clone(), fp.to_string()));
            Err(format!(
                "The {}'s certificate changed (now {fp}, was {w}), so Hopper didn't connect or send the {}. If you expect that (a reinstall or a new certificate), trust the new one in Settings → Servers.",
                what.what(),
                what.secret()
            ))
        }
        None if ask || c.vouched => Err(format!("Trust the {}'s certificate first: Settings → Servers, Edit, Save and test.", what.what())),
        None => {
            c.learned = Some(fp.to_string());
            Ok(())
        }
    }
}

/// `id`'s certificate changed since the user trusted it: (was, now).
pub fn cert_changed(what: Subject, id: &str) -> Option<(String, String)> {
    certs().get(&(what, id.to_string())).and_then(|c| c.changed.clone())
}

/// What this run found out about certificates saved before pinning, each
/// handed out once, as (id, fingerprint): Some = pin that certificate (and
/// tell the user); None = an authority signs it, so it stops learning
/// (`cert_ask`).
pub fn take_learned(what: Subject) -> Vec<(String, Option<String>)> {
    take_learned_of(what, |_| true)
}

pub(crate) fn take_learned_of(what: Subject, of: impl Fn(&str) -> bool) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for ((s, id), c) in certs().iter_mut() {
        if *s != what || c.told || !of(id) {
            continue;
        }
        if let Some(fp) = &c.learned {
            out.push((id.clone(), Some(fp.clone())));
        } else if c.vouched {
            out.push((id.clone(), None));
        } else {
            continue;
        }
        c.told = true;
    }
    out
}

/// Forgets what was seen of `id`'s certificate (its address changed, or the
/// user trusted a new one).
pub fn forget(what: Subject, id: &str) {
    certs().remove(&(what, id.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_site_and_a_panel_with_the_same_id_never_share_what_was_seen() {
        let fp = crate::tls::fingerprint(b"one");
        assert!(check(Subject::Panel, "t-same-id", "", false, &fp, false).is_ok());
        let other = crate::tls::fingerprint(b"two");
        // The panel learned `one`; the site has learned nothing, so it learns `two`.
        assert!(check(Subject::Files, "t-same-id", "", false, &other, false).is_ok());
        assert!(check(Subject::Panel, "t-same-id", "", false, &other, false).is_err());
        forget(Subject::Panel, "t-same-id");
        forget(Subject::Files, "t-same-id");
    }

    #[test]
    fn the_words_name_what_is_protected() {
        let (a, b) = (crate::tls::fingerprint(b"a"), crate::tls::fingerprint(b"b"));
        let e = check(Subject::Files, "t-words", &a, false, &b, false).unwrap_err();
        assert!(e.starts_with("The server's certificate changed") && e.contains("send the password"), "{e}");
        let e = check(Subject::Panel, "t-words", &a, false, &b, false).unwrap_err();
        assert!(e.starts_with("The panel's certificate changed") && e.contains("send the API key"), "{e}");
        let e = check(Subject::Files, "t-words2", "", true, &a, false).unwrap_err();
        assert!(e.starts_with("Trust the server's certificate first"), "{e}");
        forget(Subject::Panel, "t-words");
        forget(Subject::Files, "t-words");
    }
}
