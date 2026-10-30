//! Pterodactyl (and panels with its client API, like Pelican) with a client
//! API key (ptlc_…, from Account → API Credentials).

use super::{PanelAction, PanelServer, PanelStats, ServerPanel, agent, base_url};
use anyhow::{Context as _, bail};
use serde_json::{Value, json};

pub struct Pterodactyl {
    base: String,
    key: String,
    self_signed: bool,
    agent: ureq::Agent,
}

impl Pterodactyl {
    pub fn new(url: &str, key: &str, self_signed: bool) -> Self {
        Pterodactyl {
            base: base_url(url),
            key: key.to_string(),
            self_signed,
            agent: agent(self_signed),
        }
    }

    pub(super) fn call(&self, method: &str, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let url = format!("{}/api/client{path}", self.base);
        let auth = format!("Bearer {}", self.key);
        let accept = "Application/vnd.pterodactyl.v1+json";
        let resp = match body {
            None if method == "GET" => self
                .agent
                .get(&url)
                .header("Authorization", &auth)
                .header("Accept", accept)
                .call(),
            Some(b) => self
                .agent
                .post(&url)
                .header("Authorization", &auth)
                .header("Accept", accept)
                .send_json(b),
            None => self
                .agent
                .post(&url)
                .header("Authorization", &auth)
                .header("Accept", accept)
                .send_empty(),
        };
        let mut resp = resp.with_context(|| format!("Pterodactyl at {}", self.base))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let detail = v["errors"][0]["detail"].as_str().unwrap_or("").to_string();
        match status {
            200..=299 => Ok(v),
            401 | 403 => bail!("Pterodactyl refused the key. Use a client API key (ptlc_…) from Account → API Credentials."),
            409 => bail!("Pterodactyl says the server is busy (installing, transferring or suspended)."),
            412 => bail!("The server isn't running."),
            _ if detail.is_empty() => bail!("Pterodactyl answered {status}."),
            _ => bail!("Pterodactyl: {detail}"),
        }
    }
}

fn parse_servers(v: &Value) -> Vec<PanelServer> {
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            let a = &s["attributes"];
            let port = a["relationships"]["allocations"]["data"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| &x["attributes"])
                .find(|x| x["is_default"].as_bool().unwrap_or(false))
                .and_then(|x| x["port"].as_u64())
                .unwrap_or(0) as u16;
            PanelServer {
                id: a["identifier"].as_str().unwrap_or("").to_string(),
                name: a["name"].as_str().unwrap_or("").to_string(),
                // Pterodactyl's folders are its own volumes: port or name.
                path: String::new(),
                port,
            }
        })
        .collect()
}

fn parse_stats(v: &Value) -> PanelStats {
    let a = &v["attributes"];
    let r = &a["resources"];
    let state = a["current_state"].as_str().unwrap_or("offline");
    let mem = r["memory_bytes"].as_f64().unwrap_or(0.0);
    let uptime = r["uptime"].as_u64().unwrap_or(0) / 1000;
    PanelStats {
        running: matches!(state, "running" | "stopping"),
        starting: state == "starting",
        cpu: r["cpu_absolute"].as_f64().unwrap_or(0.0) as f32,
        mem: if mem > 0.0 { format!("{:.1}GB", mem / 1_073_741_824.0) } else { String::new() },
        started: if uptime > 0 {
            format!("up {}h {}m", uptime / 3600, uptime / 60 % 60)
        } else {
            String::new()
        },
        ..Default::default()
    }
}

fn signal(a: PanelAction) -> anyhow::Result<&'static str> {
    Ok(match a {
        PanelAction::Start => "start",
        PanelAction::Stop => "stop",
        PanelAction::Restart => "restart",
        PanelAction::Kill => "kill",
        PanelAction::Backup => bail!("Back up from the Pterodactyl panel for now."),
    })
}

impl ServerPanel for Pterodactyl {
    fn name(&self) -> &'static str {
        "Pterodactyl"
    }

    fn servers(&self) -> anyhow::Result<Vec<PanelServer>> {
        Ok(parse_servers(&self.call("GET", "", None)?))
    }

    fn stats(&self, id: &str) -> anyhow::Result<PanelStats> {
        Ok(parse_stats(&self.call("GET", &format!("/servers/{id}/resources"), None)?))
    }

    fn act(&self, id: &str, action: PanelAction) -> anyhow::Result<()> {
        let s = signal(action)?;
        self.call("POST", &format!("/servers/{id}/power"), Some(json!({ "signal": s })))
            .map(|_| ())
    }

    /// Pterodactyl streams the console over Wings' websocket only: a
    /// background connection keeps its latest lines.
    fn console(&self, id: &str) -> anyhow::Result<Vec<String>> {
        let access = super::pterodactyl_ws::Access { base: self.base.clone(), key: self.key.clone(), self_signed: self.self_signed };
        super::pterodactyl_ws::lines(&access, id)
    }

    fn command(&self, id: &str, line: &str) -> anyhow::Result<()> {
        let line = line.trim().trim_start_matches('/');
        if line.is_empty() {
            return Ok(());
        }
        self.call("POST", &format!("/servers/{id}/command"), Some(json!({ "command": line })))
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_servers_and_resources() {
        let list = parse_servers(&json!({ "data": [ { "attributes": {
            "identifier": "1a7ce997", "name": "TNP",
            "relationships": { "allocations": { "data": [
                { "attributes": { "port": 25565, "is_default": false } },
                { "attributes": { "port": 25567, "is_default": true } } ] } } } } ] }));
        assert_eq!(list[0].id, "1a7ce997");
        assert_eq!(list[0].port, 25567);
        let s = parse_stats(&json!({ "attributes": { "current_state": "running",
            "resources": { "memory_bytes": 2147483648u64, "cpu_absolute": 33.5, "uptime": 3_660_000 } } }));
        assert!(s.running);
        assert_eq!(s.mem, "2.0GB");
        assert_eq!(s.started, "up 1h 1m");
        assert!(parse_stats(&json!({ "attributes": { "current_state": "starting" } })).starting);
    }

    #[test]
    fn signals() {
        assert_eq!(signal(PanelAction::Kill).unwrap(), "kill");
        assert!(signal(PanelAction::Backup).is_err());
    }
}
