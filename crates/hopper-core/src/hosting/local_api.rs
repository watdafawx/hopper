//! Hopper Server's server routes, answered in process for a server folder
//! on this PC. Hopper desktop's server tabs talk to a [`PanelApi`]; with
//! this one, the same tabs (Worlds, Files, Players, Backups, Schedule) work
//! on a local server without a panel running. Replies have the panel's
//! shape (see crates/hopperd/README.md, "API (v1)"); the work is the same
//! hopper-core code hopperd calls.
//!
//! Only the routes those tabs use are here. Anything else answers "Hopper
//! can't do that for a server on this PC yet".

use super::backup::Kind;
use super::files;
use super::process::LocalServer;
use super::worlds;
use crate::hopperd::{Client, PanelApi, enc};
use crate::jobs::Job;
use anyhow::{Context as _, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The routes for one server folder.
pub struct LocalApi {
    root: PathBuf,
    /// The id the tabs put in paths (the folder, as text).
    id: String,
    /// The running process, when Hopper hosts the server: worlds are
    /// backed up with saving paused while it runs.
    server: Option<Arc<LocalServer>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Method {
    Get,
    Post,
    Put,
    Delete,
}

/// `%41` → `A`; `+` stays (the tabs encode spaces as `%20`).
pub fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A route's path split from its query (`a=1&b=2`, decoded).
fn split_query(path: &str) -> (&str, HashMap<String, String>) {
    match path.split_once('?') {
        Some((p, q)) => (
            p,
            q.split('&')
                .filter(|kv| !kv.is_empty())
                .map(|kv| {
                    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                    (dec(k), dec(v))
                })
                .collect(),
        ),
        None => (path, HashMap::new()),
    }
}

impl LocalApi {
    pub fn new(root: &Path, server: Option<Arc<LocalServer>>) -> Self {
        LocalApi { root: root.to_path_buf(), id: root.to_string_lossy().into_owned(), server }
    }

    /// The id to put in paths for `root` (as the tabs' `Target` does).
    pub fn id_of(root: &Path) -> String {
        root.to_string_lossy().into_owned()
    }

    fn running(&self) -> bool {
        self.server.as_ref().is_some_and(|s| s.state().is_alive())
    }

    fn stopped(&self, what: &str) -> anyhow::Result<()> {
        if self.running() {
            bail!("Stop the server before {what}.");
        }
        Ok(())
    }

    fn trash(&self) -> impl Fn(&str) -> anyhow::Result<()> + '_ {
        move |rel| files::trash(&self.root, rel).map(|_| ())
    }

    /// The route under `/servers/{id}`, as decoded segments.
    fn server_route<'a>(&self, path: &'a str) -> Option<&'a str> {
        let prefix = format!("/servers/{}", enc(&self.id));
        path.strip_prefix(&prefix).filter(|r| r.is_empty() || r.starts_with('/'))
    }

    fn route(&self, m: Method, path: &str, body: Value, job: Option<&Job>) -> anyhow::Result<Value> {
        let (path, _q) = split_query(path);
        let Some(rest) = self.server_route(path) else {
            bail!("Hopper can't do that for a server on this PC yet.");
        };
        let seg: Vec<String> = rest.split('/').filter(|s| !s.is_empty()).map(dec).collect();
        let seg: Vec<&str> = seg.iter().map(String::as_str).collect();
        let root = &self.root;
        match (m, seg.as_slice()) {
            (Method::Get, ["worlds"]) => {
                let mut v = worlds::list_json(root);
                v["running"] = json!(self.running());
                Ok(v)
            }
            (Method::Post, ["worlds"]) => {
                let name = body["name"].as_str().unwrap_or_default().trim().to_string();
                let activate = body["activate"].as_bool().unwrap_or(false);
                if activate {
                    self.stopped("switching worlds")?;
                }
                worlds::create(root, &name, body["seed"].as_str().unwrap_or_default(), activate)?;
                Ok(json!({ "ok": true }))
            }
            (Method::Post, ["worlds", name, "activate"]) => {
                self.stopped("switching worlds")?;
                worlds::set_active(root, name)?;
                Ok(json!({ "ok": true }))
            }
            (Method::Post, ["worlds", name, "backup"]) => {
                worlds::check_name(name)?;
                let quiet;
                let job = match job {
                    Some(j) => j,
                    None => {
                        quiet = crate::jobs::Jobs::default().start("backup");
                        &quiet
                    }
                };
                let b = worlds::backup_world(root, name, Kind::Manual, &format!("World {name}"), self.server.as_deref(), job)?;
                Ok(json!(b))
            }
            (Method::Delete, ["worlds", name]) => {
                worlds::delete(root, name, &self.trash())?;
                Ok(json!({ "ok": true }))
            }
            (Method::Delete, ["worlds", name, "datapacks", file]) => {
                worlds::check_name(name)?;
                worlds::check_pack_name(file).map_err(|e| anyhow::anyhow!(e))?;
                if !root.join(name).is_dir() {
                    bail!("There's no world \"{name}\".");
                }
                files::trash(root, &format!("{name}/datapacks/{file}"))?;
                Ok(json!({ "ok": true }))
            }
            _ => bail!("Hopper can't do that for a server on this PC yet."),
        }
    }
}

impl PanelApi for LocalApi {
    fn get(&self, path: &str) -> anyhow::Result<Value> {
        self.route(Method::Get, path, Value::Null, None)
    }
    fn post(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        self.route(Method::Post, path, body, None)
    }
    fn put(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        self.route(Method::Put, path, body, None)
    }
    fn delete(&self, path: &str) -> anyhow::Result<Value> {
        self.route(Method::Delete, path, Value::Null, None)
    }
    fn run_job(&self, path: &str, body: Value, job: &Job) -> anyhow::Result<Value> {
        self.route(Method::Post, path, body, Some(job))
    }
    fn upload(&self, _id: &str, local: &Path, rel: &str) -> anyhow::Result<()> {
        let dest = files::resolve(&self.root, rel)?;
        if dest.exists() {
            files::trash(&self.root, rel)?;
        }
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(local, &dest).with_context(|| format!("copying {}", local.display()))?;
        Ok(())
    }
    fn download(&self, _id: &str, rel: &str, local: &Path) -> anyhow::Result<()> {
        let src = files::resolve(&self.root, rel)?;
        if let Some(p) = local.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(&src, local).with_context(|| format!("copying {}", src.display()))?;
        Ok(())
    }
    fn client(&self) -> Option<&Client> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-localapi-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn decodes_paths() {
        assert_eq!(dec("My%20World"), "My World");
        assert_eq!(dec("a%2Fb"), "a/b");
        assert_eq!(dec("100%"), "100%");
        assert_eq!(dec("%zz"), "%zz");
        assert_eq!(split_query("/x?path=a%20b&q=").1.get("path").map(String::as_str), Some("a b"));
    }

    #[test]
    fn worlds_routes() {
        let root = temp("worlds");
        std::fs::write(root.join("server.properties"), "level-name=world\n").unwrap();
        for w in ["world", "old"] {
            std::fs::create_dir_all(root.join(w).join("datapacks")).unwrap();
            std::fs::write(root.join(w).join("level.dat"), b"").unwrap();
        }
        std::fs::write(root.join("old/datapacks/p.zip"), b"PK").unwrap();
        let api = LocalApi::new(&root, None);
        let sp = |r: &str| Client::server_path(&LocalApi::id_of(&root), r);
        let v = PanelApi::get(&api, &sp("/worlds")).unwrap();
        let names: Vec<&str> = v["worlds"].as_array().unwrap().iter().map(|w| w["name"].as_str().unwrap()).collect();
        assert_eq!(names[0], "world", "the active one first");
        assert_eq!(v["level"], "world");
        // The active world can't go; another can, and its pack first.
        assert!(PanelApi::delete(&api, &sp("/worlds/world")).is_err());
        PanelApi::delete(&api, &sp("/worlds/old/datapacks/p.zip")).unwrap();
        assert!(!root.join("old/datapacks/p.zip").exists());
        assert!(PanelApi::delete(&api, &sp("/worlds/old/datapacks/..")).is_err());
        PanelApi::post(&api, &sp("/worlds/old/activate"), Value::Null).unwrap();
        assert_eq!(worlds::level_name(&root), "old");
        PanelApi::delete(&api, &sp("/worlds/world")).unwrap();
        assert!(!root.join("world").exists());
        // Another server's id isn't this one's.
        assert!(PanelApi::get(&api, &Client::server_path("elsewhere", "/worlds")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
