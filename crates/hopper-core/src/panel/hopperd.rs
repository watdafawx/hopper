//! Hopper Server as a [`ServerPanel`]: the Servers page and Library's
//! Server tab drive it like Crafty or MCSS, through hopperd's API.

use super::{PanelAction, PanelServer, PanelStats, ServerPanel};
use crate::hopperd::Client;
use anyhow::Context as _;
use serde_json::Value;

pub struct HopperServer {
    client: Client,
}

impl HopperServer {
    pub fn new(url: &str, token: &str, ca: &str) -> Self {
        HopperServer { client: Client::new(url, token, ca) }
    }
}

fn enc(id: &str) -> String {
    id.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn mem(bytes: u64) -> String {
    if bytes == 0 {
        String::new()
    } else {
        crate::hosting::local_panel::format_bytes(bytes)
    }
}

/// hopperd's server summary as Hopper's stats.
fn stats_from(v: &Value, total_mem: u64) -> PanelStats {
    let state = v["state"].as_str().unwrap_or("stopped");
    let bytes = v["mem_bytes"].as_u64().unwrap_or(0);
    let players: Vec<String> = v["players"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(str::to_string)).collect();
    PanelStats {
        running: matches!(state, "running" | "starting" | "stopping"),
        crashed: state == "crashed",
        starting: state == "starting",
        updating: false,
        cpu: v["cpu"].as_f64().unwrap_or(0.0) as f32,
        mem: mem(bytes),
        mem_percent: if total_mem > 0 { bytes as f32 * 100.0 / total_mem as f32 } else { 0.0 },
        online: players.len() as u32,
        max: v["max_players"].as_u64().unwrap_or(20) as u32,
        players,
        version: v["version"].as_str().unwrap_or("").to_string(),
        world_size: String::new(),
        started: v["uptime_secs"]
            .as_u64()
            .map(|s| format!("up {}", crate::hosting::local_panel::uptime(s)))
            .unwrap_or_default(),
    }
}

impl ServerPanel for HopperServer {
    fn name(&self) -> &'static str {
        "Hopper Server"
    }

    fn servers(&self) -> anyhow::Result<Vec<PanelServer>> {
        let list = self.client.get("/servers")?;
        Ok(list
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| PanelServer {
                id: s["id"].as_str().unwrap_or("").to_string(),
                name: s["name"].as_str().unwrap_or("").to_string(),
                path: s["path"].as_str().unwrap_or("").to_string(),
                port: s["port"].as_u64().unwrap_or(0) as u16,
            })
            .collect())
    }

    fn stats(&self, id: &str) -> anyhow::Result<PanelStats> {
        let v = self.client.get(&format!("/servers/{}", enc(id)))?;
        let total = self.client.get("/host").ok().and_then(|h| h["mem_total"].as_u64()).unwrap_or(0);
        Ok(stats_from(&v, total))
    }

    fn act(&self, id: &str, action: PanelAction) -> anyhow::Result<()> {
        let a = match action {
            PanelAction::Start => "start",
            PanelAction::Stop => "stop",
            PanelAction::Restart => "restart",
            PanelAction::Kill => "kill",
            PanelAction::Backup => anyhow::bail!("Backups come to Hopper Server in a later version."),
        };
        self.client.post(&format!("/servers/{}/power", enc(id)), serde_json::json!({ "action": a }))?;
        Ok(())
    }

    fn console(&self, id: &str) -> anyhow::Result<Vec<String>> {
        let v = self.client.get(&format!("/servers/{}/console?since=0", enc(id)))?;
        Ok(v["lines"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_string)).collect())
    }

    fn command(&self, id: &str, line: &str) -> anyhow::Result<()> {
        self.client
            .post(&format!("/servers/{}/command", enc(id)), serde_json::json!({ "line": line }))
            .context("sending the command")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_map() {
        let v = serde_json::json!({
            "state": "running", "cpu": 12.5, "mem_bytes": 4294967296u64, "players": ["Steve", "Alex"],
            "max_players": 10, "version": "1.21.1", "uptime_secs": 3700
        });
        let s = stats_from(&v, 17179869184);
        assert_eq!(s.state(), "Running");
        assert_eq!((s.online, s.max), (2, 10));
        assert!((s.mem_percent - 25.0).abs() < 0.01);
        assert_eq!(s.started, "up 1h 01m");
        let c = stats_from(&serde_json::json!({ "state": "crashed" }), 0);
        assert_eq!(c.state(), "Crashed");
    }
}
