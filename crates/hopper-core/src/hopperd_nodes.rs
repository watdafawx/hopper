//! The nodes behind a paired Hopper Server (hopperd's multi-panel A3), as
//! Hopper desktop shows them: each node's own state for its group's light,
//! the line under its name, and whether this PC may update it. The wording
//! matches the panel's web UI (`ui/src/ServerList.svelte`, `lib/nodes.js`).

use serde::Deserialize;
use serde_json::Value;

/// `GET /api/v1/nodes/live`: the main's release and its nodes.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct NodeHeads {
    /// The hopperd release the main runs (what Update installs).
    pub version: String,
    pub nodes: Vec<NodeHead>,
}

/// One node, as the main's live feed describes it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct NodeHead {
    pub id: String,
    pub name: String,
    /// "new", "connected", "reconnecting", "offline", "update_needed" (two
    /// tunnel versions behind: read-only) or "main_older".
    pub state: String,
    /// The hopperd release the node runs.
    pub version: String,
    /// What the main may do there: "look", "operate" or "manage".
    pub ceiling: String,
    /// The main's sentence about a version gap, when there is one.
    pub skew: Option<String>,
    /// Its release is behind the main's.
    pub outdated: bool,
    /// The main can update it, and the one asking may.
    pub can_update: bool,
    pub update: Option<NodeUpdate>,
    /// Set while the node is under maintenance.
    pub maintenance: Option<Value>,
    pub last_seen: u64,
    /// The node's machine (hostname, mem_total, disk_free...), from its feed.
    pub host: Option<Value>,
}

/// An update the main asked of the node.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct NodeUpdate {
    /// "updating", "done" or "failed".
    pub state: String,
    pub target: String,
    pub message: String,
}

/// A group light's colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Light {
    Good,
    Busy,
    Bad,
}

/// How a node's line reads: plain, muted, or trouble (red).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Muted,
    Trouble,
}

impl NodeHead {
    pub fn updating(&self) -> bool {
        self.update.as_ref().is_some_and(|u| u.state == "updating")
    }

    /// Too old for the main: nothing reaches it, so its servers are
    /// read-only and nothing new goes there.
    pub fn read_only(&self) -> bool {
        self.state == "update_needed" || self.state == "main_older"
    }

    /// Green while connected and plain, red offline, amber in between (new,
    /// reconnecting, updating, under maintenance, needing an update), as
    /// the web UI's `nodeLight`.
    pub fn light(&self) -> Light {
        match self.state.as_str() {
            "offline" => Light::Bad,
            "connected" if self.maintenance.is_none() && !self.updating() => Light::Good,
            _ => Light::Busy,
        }
    }

    /// The line under the node's name, if it needs one: the web UI's
    /// health line, without the machine's numbers. `ago` turns seconds
    /// into "5 min ago".
    pub fn line(&self, now: u64, ago: impl Fn(u64) -> String) -> Option<(String, Tone)> {
        if let Some(u) = self.update.as_ref().filter(|_| self.updating()) {
            let m = if u.message.is_empty() { "Asking it to update" } else { &u.message };
            return Some((format!("Updating · {m}"), Tone::Muted));
        }
        // A node that can't be reached says so before an old failed update:
        // that's what the player has to deal with first.
        let failed = self.update.as_ref().filter(|u| u.state == "failed");
        let line = match self.state.as_str() {
            "update_needed" => ("Update needed · update it on the node itself".to_string(), Tone::Trouble),
            "main_older" => ("Newer than this panel · update this panel first".to_string(), Tone::Trouble),
            "offline" if self.last_seen > 0 => (format!("Offline · last seen {}", ago(now.saturating_sub(self.last_seen))), Tone::Trouble),
            "offline" => ("Offline".to_string(), Tone::Trouble),
            "new" => ("Node · waiting for it to connect".to_string(), Tone::Muted),
            _ if failed.is_some() => (format!("Update failed: {}", failed.map_or("", |u| u.message.as_str())), Tone::Trouble),
            _ if self.maintenance.is_some() => ("Under maintenance · nothing starts here".to_string(), Tone::Muted),
            "reconnecting" => ("Reconnecting…".to_string(), Tone::Muted),
            _ if self.can_update => ("Update available".to_string(), Tone::Muted),
            _ => return None,
        };
        Some(line)
    }
}

impl super::hopperd::Client {
    /// The main's nodes (`GET /nodes/live`). An older panel has no such
    /// route: an error, and its node groups fall back to the panel's light.
    pub fn node_heads(&self) -> anyhow::Result<NodeHeads> {
        Ok(serde_json::from_value(self.get("/nodes/live")?)?)
    }

    /// Asks the main to update node `id` to its own release. The main
    /// checks who may; the node fetches the release itself.
    pub fn update_node(&self, id: &str) -> anyhow::Result<()> {
        self.post(&format!("/nodes/{}/update", super::hopperd::enc(id)), serde_json::json!({}))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn head(v: Value) -> NodeHead {
        serde_json::from_value(v).unwrap()
    }

    fn line(h: &NodeHead) -> Option<(String, Tone)> {
        h.line(1000, |s| format!("{s} s ago"))
    }

    #[test]
    fn reads_the_mains_answer() {
        let v: NodeHeads = serde_json::from_value(json!({
            "version": "0.63.0",
            "nodes": [{ "id": "f94a", "name": "vm", "state": "update_needed", "version": "0.40.0", "skew": "vm is too old", "rtt_ms": null, "host": {} }]
        }))
        .unwrap();
        assert_eq!(v.version, "0.63.0");
        assert_eq!((v.nodes[0].id.as_str(), v.nodes[0].version.as_str()), ("f94a", "0.40.0"));
        assert!(v.nodes[0].read_only());
    }

    #[test]
    fn light_follows_the_web_ui() {
        assert_eq!(head(json!({ "state": "connected" })).light(), Light::Good);
        assert_eq!(head(json!({ "state": "offline" })).light(), Light::Bad);
        for v in [
            json!({ "state": "reconnecting" }),
            json!({ "state": "new" }),
            json!({ "state": "update_needed" }),
            json!({ "state": "connected", "maintenance": { "by": "me" } }),
            json!({ "state": "connected", "update": { "state": "updating" } }),
        ] {
            assert_eq!(head(v.clone()).light(), Light::Busy, "{v}");
        }
    }

    #[test]
    fn line_uses_the_web_ui_wording() {
        let t = |v: Value| line(&head(v)).map(|(s, _)| s);
        assert_eq!(t(json!({ "state": "update_needed" })).as_deref(), Some("Update needed · update it on the node itself"));
        assert_eq!(t(json!({ "state": "main_older" })).as_deref(), Some("Newer than this panel · update this panel first"));
        assert_eq!(
            t(json!({ "state": "connected", "update": { "state": "updating", "message": "Downloading hopperd 0.63.0" } })).as_deref(),
            Some("Updating · Downloading hopperd 0.63.0")
        );
        assert_eq!(t(json!({ "state": "connected", "update": { "state": "failed", "message": "vm: the download failed." } })).as_deref(), Some("Update failed: vm: the download failed."));
        assert_eq!(t(json!({ "state": "offline", "last_seen": 400 })).as_deref(), Some("Offline · last seen 600 s ago"));
        // Offline wins over an update that failed before it went away.
        assert_eq!(
            t(json!({ "state": "offline", "last_seen": 400, "update": { "state": "failed", "message": "x" } })).as_deref(),
            Some("Offline · last seen 600 s ago")
        );
        assert_eq!(t(json!({ "state": "connected", "can_update": true })).as_deref(), Some("Update available"));
        assert_eq!(t(json!({ "state": "connected" })), None);
        // A finished update says nothing more once the node is current.
        assert_eq!(t(json!({ "state": "connected", "update": { "state": "done", "message": "Installed" } })), None);
        assert_eq!(line(&head(json!({ "state": "update_needed" }))).unwrap().1, Tone::Trouble);
    }
}
