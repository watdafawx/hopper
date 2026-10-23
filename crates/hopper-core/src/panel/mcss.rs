//! MCSS (MC Server Soft) through its web API v2, with an API key made in
//! MCSS (Web Panel → API keys).

use super::{PanelAction, PanelServer, PanelStats, ServerPanel, agent, base_url};
use anyhow::{Context as _, bail};
use serde_json::{Value, json};

pub struct Mcss {
    base: String,
    key: String,
    agent: ureq::Agent,
}

impl Mcss {
    pub fn new(url: &str, key: &str, self_signed: bool) -> Self {
        Mcss {
            base: base_url(url),
            key: key.to_string(),
            agent: agent(self_signed),
        }
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let url = format!("{}/api/v2{path}", self.base);
        let resp = match body {
            None if method == "GET" => self.agent.get(&url).header("apiKey", &self.key).call(),
            Some(b) => self.agent.post(&url).header("apiKey", &self.key).send_json(b),
            None => self.agent.post(&url).header("apiKey", &self.key).send_empty(),
        };
        let mut resp = resp.with_context(|| format!("MCSS at {}", self.base))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        match status {
            200..=299 => Ok(serde_json::from_str(&text).unwrap_or(Value::Null)),
            401 | 403 => bail!("MCSS refused the API key. Make one in MCSS (Web Panel → API keys) with access to the servers."),
            404 => bail!("MCSS doesn't know that server, or its web API is off."),
            _ => bail!("MCSS answered {status}: {}", text.trim()),
        }
    }
}

/// MCSS's server status: 0 offline, 1 online, 2 restarting, 3 starting,
/// 4 stopping.
fn running(status: i64) -> (bool, bool) {
    (matches!(status, 1 | 2 | 4), matches!(status, 2 | 3))
}

fn parse_servers(v: &Value) -> Vec<PanelServer> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|s| PanelServer {
            id: s["serverId"].as_str().unwrap_or("").to_string(),
            name: s["name"].as_str().unwrap_or("").to_string(),
            path: s["pathToFolder"]
                .as_str()
                .or(s["folderName"].as_str())
                .unwrap_or("")
                .to_string(),
            // MCSS doesn't say; the folder or name has to match.
            port: 0,
        })
        .collect()
}

fn parse_stats(server: &Value, stats: &Value) -> PanelStats {
    let l = &stats["latest"];
    let n = |v: &Value| v.as_f64().unwrap_or(0.0);
    let (run, starting) = running(server["status"].as_i64().unwrap_or(0));
    let (used, limit) = (n(&l["memoryUsed"]), n(&l["memoryLimit"]));
    PanelStats {
        running: run,
        starting,
        cpu: n(&l["cpu"]) as f32,
        mem: if used > 0.0 { format!("{:.1}GB", used / 1024.0) } else { String::new() },
        mem_percent: if limit > 0.0 { (used / limit * 100.0) as f32 } else { 0.0 },
        online: n(&l["playersOnline"]) as u32,
        max: n(&l["playerLimit"]) as u32,
        started: l["startDate"].as_str().unwrap_or("").to_string(),
        ..Default::default()
    }
}

fn action_code(a: PanelAction) -> anyhow::Result<u8> {
    Ok(match a {
        PanelAction::Stop => 1,
        PanelAction::Start => 2,
        PanelAction::Kill => 3,
        PanelAction::Restart => 4,
        PanelAction::Backup => bail!("Back up from MCSS itself for now."),
    })
}

impl ServerPanel for Mcss {
    fn name(&self) -> &'static str {
        "MCSS"
    }

    fn servers(&self) -> anyhow::Result<Vec<PanelServer>> {
        Ok(parse_servers(&self.call("GET", "/servers", None)?))
    }

    fn stats(&self, id: &str) -> anyhow::Result<PanelStats> {
        let server = self.call("GET", &format!("/servers/{id}"), None)?;
        let stats = self.call("GET", &format!("/servers/{id}/stats"), None)?;
        Ok(parse_stats(&server, &stats))
    }

    fn act(&self, id: &str, action: PanelAction) -> anyhow::Result<()> {
        let code = action_code(action)?;
        self.call("POST", &format!("/servers/{id}/execute/action"), Some(json!({ "action": code })))
            .map(|_| ())
    }

    fn console(&self, id: &str) -> anyhow::Result<Vec<String>> {
        let v = self.call(
            "GET",
            &format!("/servers/{id}/console?AmountOfLines=300&Reversed=false&takeFromBeginning=false"),
            None,
        )?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l.as_str().map(String::from))
            .collect())
    }

    fn command(&self, id: &str, line: &str) -> anyhow::Result<()> {
        let line = line.trim().trim_start_matches('/');
        if line.is_empty() {
            return Ok(());
        }
        self.call("POST", &format!("/servers/{id}/execute/command"), Some(json!({ "command": line })))
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_servers_and_stats() {
        let list = parse_servers(&json!([
            { "serverId": "a-1", "name": "TNP", "pathToFolder": "G:\\MCSS\\servers\\TNP-LL8", "status": 1 }
        ]));
        assert_eq!(list[0].id, "a-1");
        assert!(list[0].path.ends_with("TNP-LL8"));
        let s = parse_stats(
            &json!({ "status": 1 }),
            &json!({ "latest": { "cpu": 12, "memoryUsed": 4096, "memoryLimit": 8192,
                                 "playersOnline": 3, "playerLimit": 20, "startDate": "2026-10-12" } }),
        );
        assert!(s.running && !s.starting);
        assert_eq!((s.online, s.max), (3, 20));
        assert_eq!(s.mem, "4.0GB");
        assert_eq!(s.mem_percent, 50.0);
        assert!(parse_stats(&json!({ "status": 3 }), &json!({})).starting);
    }

    #[test]
    fn actions_map_to_mcss_codes() {
        assert_eq!(action_code(PanelAction::Start).unwrap(), 2);
        assert_eq!(action_code(PanelAction::Kill).unwrap(), 3);
        assert!(action_code(PanelAction::Backup).is_err());
    }
}
