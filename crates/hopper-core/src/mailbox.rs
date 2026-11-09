//! The mailbox on the internet: pairing and recipe delivery for PCs that
//! can't reach each other directly.
//!
//! Only share codes travel here. Files still come from CurseForge, Modrinth,
//! GitHub and the other sources on each PC. The account is a random id and
//! bearer token kept in Hopper's data folder; the server holds no files.

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Where the public mailbox is. Change for a self-hosted one.
pub const DEFAULT_URL: &str = "https://sync.kyuubi.dev";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub id: String,
    pub token: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Friend {
    pub id: String,
    pub name: String,
    pub online: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Event {
    pub seq: u64,
    pub from: String,
    pub from_name: String,
    pub inst: String,
    pub name: String,
    pub code: String,
    pub left_out: Vec<String>,
    pub at: u64,
}

pub struct Mailbox {
    base: String,
    account: Account,
    agent: ureq::Agent,
}

impl Mailbox {
    /// Loads this PC's account, registering it the first time.
    pub fn load(base: &str, name: &str) -> anyhow::Result<Self> {
        let base = base.trim_end_matches('/').to_string();
        let path = account_path();
        let mut account: Account = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(90)))
            .http_status_as_error(false)
            .build()
            .into();
        let me = Self {
            base,
            account: account.clone(),
            agent,
        };
        if account.id.is_empty() || account.token.is_empty() {
            let v = me.post("/v1/register", None, serde_json::json!({ "name": name }))?;
            account = Account {
                id: v["id"].as_str().context("mailbox: no id")?.into(),
                token: v["token"].as_str().context("mailbox: no token")?.into(),
                name: v["name"].as_str().unwrap_or(name).into(),
            };
            if let Some(d) = path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::write(&path, serde_json::to_vec_pretty(&account)?)
                .with_context(|| format!("saving {}", path.display()))?;
            return Ok(Self {
                base: me.base,
                account,
                agent: me.agent,
            });
        }
        Ok(me)
    }

    /// A fresh account, not saved. Tests use two of these to pair.
    pub fn register(base: &str, name: &str) -> anyhow::Result<Self> {
        let base = base.trim_end_matches('/').to_string();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(90)))
            .http_status_as_error(false)
            .build()
            .into();
        let mut me = Self {
            base,
            account: Account::default(),
            agent,
        };
        let v = me.post("/v1/register", None, serde_json::json!({ "name": name }))?;
        me.account = Account {
            id: v["id"].as_str().context("mailbox: no id")?.into(),
            token: v["token"].as_str().context("mailbox: no token")?.into(),
            name: v["name"].as_str().unwrap_or(name).into(),
        };
        Ok(me)
    }

    /// The saved account, for showing this PC's name.
    pub fn account(&self) -> &Account {
        &self.account
    }

    pub fn invite(&self) -> anyhow::Result<String> {
        let v = self.post("/v1/invite", Some(&self.account.token), serde_json::json!({}))?;
        Ok(v["code"].as_str().context("mailbox: no code")?.to_string())
    }

    pub fn join(&self, code: &str) -> anyhow::Result<Friend> {
        let v = self.post(
            "/v1/join",
            Some(&self.account.token),
            serde_json::json!({ "code": code.trim().to_uppercase() }),
        )?;
        Ok(serde_json::from_value(v["friend"].clone()).unwrap_or_default())
    }

    pub fn friends(&self) -> anyhow::Result<Vec<Friend>> {
        let v = self.get("/v1/friends", Some(&self.account.token))?;
        Ok(serde_json::from_value(v["friends"].clone()).unwrap_or_default())
    }

    pub fn remove(&self, id: &str) -> anyhow::Result<()> {
        self.post(
            "/v1/remove",
            Some(&self.account.token),
            serde_json::json!({ "id": id }),
        )?;
        Ok(())
    }

    pub fn publish(
        &self,
        to: &str,
        inst: &str,
        name: &str,
        code: &str,
        left_out: &[String],
    ) -> anyhow::Result<u64> {
        let v = self.post(
            "/v1/recipes",
            Some(&self.account.token),
            serde_json::json!({
                "to": to,
                "inst": inst,
                "name": name,
                "code": code,
                "left_out": left_out,
            }),
        )?;
        Ok(v["seq"].as_u64().unwrap_or(0))
    }

    /// Recipes published to this PC after `after`, waiting up to `wait`
    /// seconds for one. Returns the newest sequence seen.
    pub fn events(&self, after: u64, wait: u64) -> anyhow::Result<(u64, Vec<Event>)> {
        let v = self.get(
            &format!("/v1/recipes?after={after}&wait={wait}"),
            Some(&self.account.token),
        )?;
        Ok((
            v["seq"].as_u64().unwrap_or(after),
            serde_json::from_value(v["events"].clone()).unwrap_or_default(),
        ))
    }

    fn get(&self, path: &str, token: Option<&str>) -> anyhow::Result<serde_json::Value> {
        let mut r = self.agent.get(format!("{}{}", self.base, path));
        if let Some(t) = token {
            r = r.header("Authorization", format!("Bearer {t}"));
        }
        let mut resp = r.call().with_context(|| format!("mailbox: GET {path}"))?;
        let status = resp.status().as_u16();
        let v: serde_json::Value = resp.body_mut().read_json()?;
        if status >= 400 {
            bail!("{}", v["error"].as_str().unwrap_or("mailbox error"));
        }
        Ok(v)
    }

    fn post(
        &self,
        path: &str,
        token: Option<&str>,
        body: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let mut r = self.agent.post(format!("{}{}", self.base, path));
        if let Some(t) = token {
            r = r.header("Authorization", format!("Bearer {t}"));
        }
        let mut resp = r.send_json(body).with_context(|| format!("mailbox: POST {path}"))?;
        let status = resp.status().as_u16();
        let v: serde_json::Value = resp.body_mut().read_json()?;
        if status >= 400 || v.get("error").is_some() {
            bail!("{}", v["error"].as_str().unwrap_or("mailbox error"));
        }
        Ok(v)
    }
}

fn account_path() -> PathBuf {
    crate::paths::dirs()
        .map(|d| d.data_dir().join("mailbox.json"))
        .unwrap_or_else(|| std::env::temp_dir().join("hopper-mailbox.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_round_trips() {
        let a = Account {
            id: "a".repeat(32),
            token: "t".repeat(43),
            name: "FOX".into(),
        };
        let b: Account = serde_json::from_slice(&serde_json::to_vec(&a).unwrap()).unwrap();
        assert_eq!((b.id, b.token, b.name), (a.id, a.token, a.name));
    }

    #[test]
    fn events_keep_their_shape() {
        let e: Event = serde_json::from_str(
            r#"{"seq":3,"from":"aa","from_name":"FOX","inst":"TNP","name":"ATM","code":"HOP-x","left_out":["a.jar"],"at":1}"#,
        )
        .unwrap();
        assert_eq!(e.from_name, "FOX");
        assert_eq!(e.left_out, ["a.jar"]);
    }

    /// The same calls the Friends buttons make. Needs a running mailbox:
    /// `HOPPER_MAILBOX_URL=https://... cargo test -p hopper-core --lib
    /// mailbox::tests::two_accounts_pair_and_publish -- --ignored`.
    #[test]
    #[ignore = "needs a live mailbox (HOPPER_MAILBOX_URL)"]
    fn two_accounts_pair_and_publish() {
        let base = std::env::var("HOPPER_MAILBOX_URL").expect("HOPPER_MAILBOX_URL");
        let a = Mailbox::register(&base, "hopper-test-a").unwrap();
        let b = Mailbox::register(&base, "hopper-test-b").unwrap();
        let code = a.invite().unwrap();
        let friend = b.join(&code).unwrap();
        assert_eq!(friend.name, "hopper-test-a");
        assert!(a
            .friends()
            .unwrap()
            .iter()
            .any(|f| f.id == b.account().id));
        a.publish(&b.account().id, "TNP", "ATM", "HOP-test", &[])
            .unwrap();
        let (_, events) = b.events(0, 5).unwrap();
        assert!(events
            .iter()
            .any(|e| e.from == a.account().id && e.inst == "TNP"));
    }
}
