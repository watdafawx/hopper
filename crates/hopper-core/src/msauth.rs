//! Microsoft sign-in for Minecraft: device code → Microsoft token → Xbox
//! Live → XSTS → Minecraft services. The refresh token lives in Windows
//! Credential Manager; nothing secret is written to disk by Hopper.
//!
//! Needs an Azure app id approved for the Minecraft API, from `.env`
//! (`MS_CLIENT_ID`). Mojang rejects ids it has not approved.

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SCOPE: &str = "XboxLive.signin offline_access";
const KEYRING_SERVICE: &str = "Hopper Minecraft";

/// The Azure app id: MS_CLIENT_ID (.env or the environment), else the one
/// built into this exe.
pub fn client_id() -> Option<String> {
    std::env::var("MS_CLIENT_ID")
        .ok()
        .or_else(|| option_env!("HOPPER_MS_CLIENT_ID").map(String::from))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// An agent that hands back 4xx bodies (the token endpoint answers
/// "authorization_pending" with a 400).
fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .user_agent(concat!("Hopper/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    })
}

fn read_json(mut r: ureq::http::Response<ureq::Body>) -> anyhow::Result<(u16, Value)> {
    let status = r.status().as_u16();
    let v = r.body_mut().read_json::<Value>().unwrap_or(Value::Null);
    Ok((status, v))
}

/// What to show while the user signs in.
#[derive(Clone, Debug)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    device_code: String,
    interval: u64,
    expires: Instant,
}

pub fn start_device_code() -> anyhow::Result<DeviceCode> {
    let id = client_id()
        .context("Add MS_CLIENT_ID=... to .env (an Azure app approved for Minecraft)")?;
    let r = agent()
        .post("https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode")
        .send_form([("client_id", id.as_str()), ("scope", SCOPE)])?;
    let (status, v) = read_json(r)?;
    if status != 200 {
        bail!(
            "Microsoft refused the sign-in: {}",
            v["error_description"].as_str().unwrap_or("unknown error")
        );
    }
    Ok(DeviceCode {
        user_code: v["user_code"].as_str().unwrap_or_default().to_string(),
        verification_uri: v["verification_uri"]
            .as_str()
            .unwrap_or("https://microsoft.com/link")
            .to_string(),
        device_code: v["device_code"].as_str().unwrap_or_default().to_string(),
        interval: v["interval"].as_u64().unwrap_or(5),
        expires: Instant::now() + Duration::from_secs(v["expires_in"].as_u64().unwrap_or(900)),
    })
}

/// Waits for the user to finish in the browser; returns the refresh token
/// and Microsoft access token.
pub fn poll_device_code(dc: &DeviceCode) -> anyhow::Result<(String, String)> {
    let id = client_id().context("no MS_CLIENT_ID")?;
    let mut interval = dc.interval;
    loop {
        if Instant::now() > dc.expires {
            bail!("The sign-in code expired; start again.");
        }
        std::thread::sleep(Duration::from_secs(interval));
        let r = agent()
            .post("https://login.microsoftonline.com/consumers/oauth2/v2.0/token")
            .send_form([
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", id.as_str()),
                ("device_code", dc.device_code.as_str()),
            ])?;
        let (status, v) = read_json(r)?;
        if status == 200 {
            return Ok((
                v["refresh_token"].as_str().unwrap_or_default().to_string(),
                v["access_token"].as_str().unwrap_or_default().to_string(),
            ));
        }
        match v["error"].as_str().unwrap_or("") {
            "authorization_pending" => {}
            "slow_down" => interval += 5,
            "authorization_declined" => bail!("Sign-in was declined."),
            "expired_token" => bail!("The sign-in code expired; start again."),
            other => bail!(
                "Sign-in failed: {other} {}",
                v["error_description"].as_str().unwrap_or("")
            ),
        }
    }
}

/// New Microsoft access token from a refresh token (which may rotate).
fn refresh_ms(refresh: &str) -> anyhow::Result<(String, String)> {
    let id = client_id().context("no MS_CLIENT_ID")?;
    let r = agent()
        .post("https://login.microsoftonline.com/consumers/oauth2/v2.0/token")
        .send_form([
            ("grant_type", "refresh_token"),
            ("client_id", id.as_str()),
            ("refresh_token", refresh),
            ("scope", SCOPE),
        ])?;
    let (status, v) = read_json(r)?;
    if status != 200 {
        bail!(
            "Your Microsoft sign-in expired; sign in again. ({})",
            v["error"].as_str().unwrap_or("?")
        );
    }
    Ok((
        v["refresh_token"].as_str().unwrap_or(refresh).to_string(),
        v["access_token"].as_str().unwrap_or_default().to_string(),
    ))
}

#[derive(Clone, Debug, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
}

/// A ready-to-launch Minecraft session. The access token is kept in memory only.
#[derive(Clone)]
pub struct Session {
    pub profile: Profile,
    pub access_token: String,
    pub xuid: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

/// Whether `/entitlements/mcstore` lists Java Edition: `product_minecraft`
/// or `game_minecraft` (Game Pass gives these too).
fn owns_java(v: &Value) -> bool {
    v["items"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|i| matches!(i["name"].as_str(), Some("product_minecraft" | "game_minecraft")))
    })
}

/// Microsoft token → Minecraft session.
pub fn minecraft_session(ms_access: &str) -> anyhow::Result<Session> {
    let (s, v) = read_json(agent().post("https://user.auth.xboxlive.com/user/authenticate").send_json(json!({
        "Properties": { "AuthMethod": "RPS", "SiteName": "user.auth.xboxlive.com", "RpsTicket": format!("d={ms_access}") },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT",
    }))?)?;
    if s != 200 {
        bail!("Xbox Live sign-in failed ({s})");
    }
    let xbl = v["Token"].as_str().unwrap_or_default().to_string();
    let uhs = v["DisplayClaims"]["xui"][0]["uhs"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let (s, v) = read_json(
        agent()
            .post("https://xsts.auth.xboxlive.com/xsts/authorize")
            .send_json(json!({
                "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbl] },
                "RelyingParty": "rp://api.minecraftservices.com/",
                "TokenType": "JWT",
            }))?,
    )?;
    if s != 200 {
        let why = match v["XErr"].as_u64() {
            Some(2148916233) => {
                "This Microsoft account has no Xbox profile. Sign in once at minecraft.net to create one."
            }
            Some(2148916235) => "Xbox Live isn't available in this account's country.",
            Some(2148916236 | 2148916237) => "This account needs adult verification on xbox.com.",
            Some(2148916238) => {
                "This is a child account; an adult has to add it to a Microsoft family."
            }
            _ => "Xbox security token was refused.",
        };
        bail!("{why}");
    }
    let xsts = v["Token"].as_str().unwrap_or_default().to_string();
    let xuid = v["DisplayClaims"]["xui"][0]["xid"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let (s, v) = read_json(
        agent()
            .post("https://api.minecraftservices.com/authentication/login_with_xbox")
            .send_json(json!({ "identityToken": format!("XBL3.0 x={uhs};{xsts}") }))?,
    )?;
    if s != 200 {
        bail!(
            "Minecraft refused the sign-in ({s}). If this is a new app id, Mojang has to approve it for the Minecraft API first."
        );
    }
    let access = v["access_token"].as_str().unwrap_or_default().to_string();

    // Does the account own Java Edition (bought, or through Game Pass)?
    let (s, v) = read_json(
        agent()
            .get("https://api.minecraftservices.com/entitlements/mcstore")
            .header("Authorization", format!("Bearer {access}"))
            .call()?,
    )?;
    let owns = s == 200 && owns_java(&v);
    if s == 200 && !owns {
        bail!("This account doesn't own Minecraft: Java Edition (bought or through Game Pass).");
    }

    let r = agent()
        .get("https://api.minecraftservices.com/minecraft/profile")
        .header("Authorization", format!("Bearer {access}"))
        .call()?;
    let (s, v) = read_json(r)?;
    if s == 404 && owns {
        bail!("This account owns Minecraft but has no player name yet. Open minecraft.net (or the Minecraft Launcher) once to pick one, then sign in again.");
    }
    if s == 404 {
        bail!("This account doesn't own Minecraft: Java Edition.");
    }
    if s != 200 {
        bail!("Could not read the Minecraft profile ({s})");
    }
    Ok(Session {
        profile: serde_json::from_value(v)?,
        access_token: access,
        xuid,
    })
}

// ── refresh tokens in Windows Credential Manager ────────────────────

/// Hopper's first single sign-in kept its token here. Accounts are now
/// saved per profile ([`crate::accounts`]); this one is read once, copied,
/// and left in place.
fn legacy_entry() -> anyhow::Result<keyring::Entry> {
    Ok(keyring::Entry::new(KEYRING_SERVICE, "refresh_token")?)
}

/// The refresh token of the old single sign-in, if one is saved.
pub fn legacy_refresh() -> Option<String> {
    legacy_entry().ok()?.get_password().ok()
}

/// Forgets the old single sign-in (when the user signs that account out).
pub fn delete_legacy() -> anyhow::Result<()> {
    match legacy_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// The token entry of one account, by profile id.
pub(crate) fn account_entry(profile_id: &str) -> anyhow::Result<keyring::Entry> {
    Ok(keyring::Entry::new(
        KEYRING_SERVICE,
        &format!("refresh_token:{profile_id}"),
    )?)
}

/// A session from a refresh token, and the token to keep (it may rotate).
pub fn session_from_refresh(refresh: &str) -> anyhow::Result<(String, Session)> {
    let (refresh, access) = refresh_ms(refresh)?;
    Ok((refresh, minecraft_session(&access)?))
}

/// Whether there is an account to launch with.
pub fn has_saved_login() -> bool {
    crate::accounts::has_login()
}

/// A fresh session for the active account.
pub fn session_from_saved() -> anyhow::Result<Session> {
    crate::accounts::active_session()
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_entitlements() {
        let owned = serde_json::json!({ "items": [
            { "name": "product_minecraft", "signature": "x" },
            { "name": "game_minecraft", "signature": "y" }
        ], "signature": "z", "keyId": "1" });
        assert!(super::owns_java(&owned));
        let bedrock_only = serde_json::json!({ "items": [{ "name": "product_minecraft_bedrock" }] });
        assert!(!super::owns_java(&bedrock_only));
        assert!(!super::owns_java(&serde_json::json!({ "items": [] })));
    }
}
