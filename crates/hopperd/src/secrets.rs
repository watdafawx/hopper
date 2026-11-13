//! Passwords in a server's files (design/server-secrets.md).
//!
//! `server.properties` holds the RCON password and, on newer Minecraft, the
//! management server's secret. Whoever knows them can run any command on
//! the server without the panel, so only people who may change the
//! server's files see them: they could set a new password anyway. Everyone
//! else (a Viewer or Operator, a read-only token, a node request at a lower
//! ceiling) gets the settings without the secret keys and can't open the
//! file, its saved-over copies or Hopper's own folders, nor download it in
//! a zip or a backup.

use crate::auth::Who;
use axum::http::Method;

/// A server.properties key whose value is a password or a secret:
/// `rcon.password`, `management-server-secret`,
/// `management-server-tls-keystore-password` and any later key named like
/// them. The same list config files are masked with, shared with desktop.
pub fn is_secret_key(key: &str) -> bool {
    hopper_core::configedit::secret_key(key)
}

/// A file holding them: any `server.properties`, wherever it is (a copy in
/// a subfolder or a pack's template is often the real one renamed).
pub fn is_secret_file(rel: &str) -> bool {
    parts(rel).last().is_some_and(|n| n.eq_ignore_ascii_case("server.properties"))
}

/// A path's folder and file names the way Windows reads them: no empty or
/// `.` parts, and no trailing dots or spaces (`server.properties.` opens
/// server.properties).
fn parts(rel: &str) -> Vec<String> {
    rel.split(['/', '\\']).map(|p| p.trim_end_matches(['.', ' ']).to_string()).filter(|p| !p.is_empty()).collect()
}

fn in_internal(rel: &str) -> bool {
    parts(rel).iter().any(|p| crate::files::is_internal(p))
}

/// A path someone without file rights may not read: a secret file, or
/// anything in Hopper's own folders (the trash keeps saved-over copies of
/// server.properties, the backups folder whole servers).
pub fn is_hidden_path(rel: &str) -> bool {
    is_secret_file(rel) || in_internal(rel)
}

/// `rel` in the server folder as the file system names it (a short 8.3
/// name or other case becomes the real name), when it exists.
fn real_rel(root: &std::path::Path, rel: &str) -> Option<String> {
    let p = crate::files::resolve(root, rel).ok()?;
    let real = std::fs::canonicalize(p).ok()?;
    let base = std::fs::canonicalize(root).ok()?;
    Some(crate::files::rel_of(&base, &real))
}

/// `is_hidden_path` for what `rel` really opens in `root` too.
fn hidden_in(root: Option<&std::path::Path>, rel: &str, only_folders: bool) -> bool {
    let check = |r: &str| if only_folders { in_internal(r) } else { is_hidden_path(r) };
    check(rel) || root.and_then(|root| real_rel(root, rel)).is_some_and(|r| check(&r))
}

pub const MSG: &str = "Only people who can change this server's files can open server.properties, Hopper's own folders or whole backups: they hold the RCON password.";

/// Why `who` may not make this read (`path` and the query of a request),
/// when it's one that would show a secret. Called from the guard for
/// direct and relayed requests. `root` is the server's folder when it's
/// on this panel, so a path is also checked as the file it really opens.
pub fn refusal(who: &Who, method: &Method, path: &str, query: Option<&str>, root: Option<&std::path::Path>) -> Option<&'static str> {
    if who.sees_secrets() || !(method == Method::GET || method == Method::HEAD) {
        return None;
    }
    if let Some(rest) = path.strip_prefix("/api/v1/servers/") {
        // A backup is a whole copy of the server, server.properties and all.
        let mut parts = rest.split('/');
        let (_, a, _, d) = (parts.next(), parts.next(), parts.next(), parts.next());
        return (a == Some("backups") && d == Some("download")).then_some(MSG);
    }
    let rest = path.strip_prefix("/api/v1/fs/")?;
    let route = rest.split_once('/').map_or("", |(_, r)| r);
    // History's "Use this version" names a saved-over copy by its id only.
    if route == "version-text" {
        return Some(MSG);
    }
    if !matches!(route, "file" | "text" | "diff" | "history" | "zip-list" | "zip-entry" | "zip" | "search" | "list" | "walk") {
        return None;
    }
    let pairs = query_pairs(query);
    // A folder listing or a search may start in a normal folder; what they
    // find inside Hopper's folders is left out by the routes themselves.
    // Listing a folder only shows names; a search or a zip of a folder
    // leaves out what's secret inside (`shows`).
    let folders_only = matches!(route, "list" | "walk");
    let bad = pairs.iter().any(|(k, v)| match k.as_str() {
        "path" => hidden_in(root, v, folders_only),
        "entry" => is_secret_file(v),
        _ => false,
    });
    bad.then_some(MSG)
}

/// The same check for a one-use link's files (`entry`: one entry of the
/// zip at `paths[0]`; a backup is `backup`).
pub fn link_refusal(who: &Who, root: &std::path::Path, paths: &[String], entry: Option<&str>, backup: bool) -> Option<&'static str> {
    if who.sees_secrets() {
        return None;
    }
    // A folder in a zip is fine: the zip leaves out what's secret inside.
    let bad = backup || entry.is_some_and(is_secret_file) || paths.iter().any(|p| hidden_in(Some(root), p, false));
    bad.then_some(MSG)
}

/// The query decoded the way the routes' `Query` extractors decode it. A
/// query that doesn't decode is refused by those routes too; here it's
/// treated as naming every path, so nothing slips past.
fn query_pairs(query: Option<&str>) -> Vec<(String, String)> {
    let Some(q) = query else { return Vec::new() };
    let uri: Option<axum::http::Uri> = format!("/?{q}").parse().ok();
    match uri.and_then(|u| axum::extract::Query::<Vec<(String, String)>>::try_from_uri(&u).ok()) {
        Some(axum::extract::Query(p)) => p,
        None => vec![("path".into(), "server.properties".into())],
    }
}

/// Whether `who` may see a hit or zip entry at `rel` (a search result, a
/// file in a zip).
pub fn shows(who: &Who, rel: &str) -> bool {
    who.sees_secrets() || !is_hidden_path(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;

    fn viewer() -> Who {
        Who::User { id: "v".into(), name: "v".into(), role: Role::Viewer, servers: vec![] }
    }

    #[test]
    fn secret_keys() {
        for k in ["rcon.password", "management-server-secret", "management-server-tls-keystore-password", "RCON.Password"] {
            assert!(is_secret_key(k), "{k}");
        }
        for k in ["motd", "rcon.port", "enable-rcon", "level-name", "max-players", "management-server-port", "enforce-secure-profile", "online-mode", "server-ip"] {
            assert!(!is_secret_key(k), "{k}");
        }
        // No ordinary setting of the Settings page is hidden by mistake.
        for spec in hopper_core::hosting::properties::COMMON {
            assert!(!is_secret_key(spec.key) || spec.key.contains("password") || spec.key.contains("secret"), "{}", spec.key);
        }
    }

    #[test]
    fn hidden_paths() {
        assert!(is_secret_file("server.properties") && is_secret_file("old/Server.Properties") && is_secret_file("a\\server.properties"));
        assert!(!is_secret_file("config/server.properties.txt") && !is_secret_file("mods"));
        assert!(is_hidden_path(".hopper-trash/abc/item") && is_hidden_path("./.hopper-server-backups/x.zip"));
        assert!(!is_hidden_path("config/create.toml") && !is_hidden_path(""));
    }

    #[test]
    fn lookers_refused_secret_reads() {
        let v = viewer();
        let g = Method::GET;
        let no = |p: &str, q: Option<&str>| refusal(&v, &g, p, q, None).is_some();
        assert!(no("/api/v1/fs/smp/text", Some("path=server.properties")));
        assert!(no("/api/v1/fs/smp/file", Some("path=%2Fserver.properties")));
        assert!(no("/api/v1/fs/smp/file", Some("path=.hopper-trash%2Fx%2Fitem")));
        assert!(no("/api/v1/fs/smp/diff", Some("path=server.properties")));
        assert!(no("/api/v1/fs/smp/zip", Some("path=.hopper-server-backups")));
        assert!(no("/api/v1/fs/smp/zip-entry", Some("path=pack.zip&entry=server.properties")));
        assert!(no("/api/v1/fs/smp/version-text", Some("version=abc")));
        assert!(no("/api/v1/fs/smp/text", Some("path=server.properties.%20")));
        assert!(no("/api/v1/fs/smp/text", Some("path=.%5Cserver.properties%2F.")));
        assert!(no("/api/v1/fs/smp/search", Some("path=.hopper-trash")));
        assert!(no("/api/v1/servers/smp/backups/b1.zip/download", None));
        // Ordinary reads stay open.
        assert!(!no("/api/v1/fs/smp/text", Some("path=config%2Fcreate.toml")));
        assert!(!no("/api/v1/fs/smp/zip", Some("path=")));
        assert!(!no("/api/v1/fs/smp/list", Some("path=")));
        assert!(!no("/api/v1/servers/smp/backups", None));
        assert!(!no("/api/v1/servers/smp/properties", None));
        // Someone with file rights reads it all.
        let admin = Who::User { id: "a".into(), name: "a".into(), role: Role::Admin, servers: vec![] };
        assert!(refusal(&admin, &g, "/api/v1/fs/smp/text", Some("path=server.properties"), None).is_none());
        assert!(refusal(&Who::Owner, &g, "/api/v1/servers/smp/backups/b/download", None, None).is_none());
    }
}
