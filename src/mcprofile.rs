//! The player's Minecraft profile through the Minecraft Services API: the
//! name, skins and capes, and changing them. Every call takes the Minecraft
//! access token of a signed-in [`crate::msauth::Session`].

use anyhow::{Context as _, bail};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;

const API: &str = "https://api.minecraftservices.com/minecraft/profile";

/// Days Mojang makes you wait between name changes.
pub const NAME_COOLDOWN_DAYS: i64 = 30;

/// Classic (4 pixel) or slim (3 pixel, "Alex") arms.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Variant {
    #[default]
    Classic,
    Slim,
}

impl Variant {
    /// What the API takes: "classic" or "slim".
    pub fn api(self) -> &'static str {
        match self {
            Variant::Classic => "classic",
            Variant::Slim => "slim",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Variant::Classic => "Classic",
            Variant::Slim => "Slim",
        }
    }

    pub fn is_slim(self) -> bool {
        self == Variant::Slim
    }
}

impl<'de> Deserialize<'de> for Variant {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(if s.eq_ignore_ascii_case("slim") {
            Variant::Slim
        } else {
            Variant::Classic
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Skin {
    pub id: String,
    /// "ACTIVE" or "INACTIVE".
    #[serde(default)]
    pub state: String,
    pub url: String,
    #[serde(default)]
    pub variant: Variant,
    #[serde(default)]
    pub texture_key: String,
    #[serde(default)]
    pub alias: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Cape {
    pub id: String,
    #[serde(default)]
    pub state: String,
    pub url: String,
    /// Its name, like "Migrator" or "Pan".
    #[serde(default)]
    pub alias: String,
}

impl Cape {
    pub fn is_active(&self) -> bool {
        self.state.eq_ignore_ascii_case("active")
    }
}

/// The profile as `GET /minecraft/profile` returns it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub skins: Vec<Skin>,
    #[serde(default)]
    pub capes: Vec<Cape>,
}

impl McProfile {
    /// The skin the player wears (the default one when none is active).
    pub fn active_skin(&self) -> Option<&Skin> {
        self.skins
            .iter()
            .find(|s| s.state.eq_ignore_ascii_case("active"))
            .or(self.skins.first())
    }

    pub fn active_cape(&self) -> Option<&Cape> {
        self.capes.iter().find(|c| c.is_active())
    }
}

/// When the name may change, from `GET /minecraft/profile/namechange`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct NameChange {
    pub changed_at: Option<String>,
    pub created_at: Option<String>,
    pub name_change_allowed: bool,
}

impl NameChange {
    /// Seconds since the epoch when the next change is allowed, while it
    /// isn't yet.
    pub fn next_change(&self) -> Option<i64> {
        if self.name_change_allowed {
            return None;
        }
        let changed = crate::time::parse_iso(self.changed_at.as_deref()?)?;
        Some(changed + NAME_COOLDOWN_DAYS * 86_400)
    }

    /// One sentence for the UI.
    pub fn describe(&self, now: i64) -> String {
        if self.name_change_allowed {
            return "You can change your name now.".into();
        }
        match self.next_change() {
            Some(t) if t > now => {
                let days = (t - now + 86_399) / 86_400;
                format!(
                    "You can change it again on {} (in {days} day{}).",
                    crate::time::date(t),
                    if days == 1 { "" } else { "s" }
                )
            }
            Some(_) => "Mojang hasn't allowed a change yet; try again later.".into(),
            None => "Mojang doesn't allow this account to change its name right now.".into(),
        }
    }
}

/// Whether a name is free, from `GET /minecraft/profile/name/{name}/available`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameStatus {
    Available,
    /// Someone has it.
    Taken,
    /// Mojang's filter blocks it.
    NotAllowed,
}

impl NameStatus {
    fn parse(v: &Value) -> anyhow::Result<Self> {
        match v["status"].as_str().unwrap_or_default() {
            "AVAILABLE" => Ok(NameStatus::Available),
            "DUPLICATE" => Ok(NameStatus::Taken),
            "NOT_ALLOWED" => Ok(NameStatus::NotAllowed),
            other => bail!("Mojang answered something unexpected about that name ({other})."),
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            NameStatus::Available => "Available",
            NameStatus::Taken => "Someone already has this name",
            NameStatus::NotAllowed => "Mojang doesn't allow this name",
        }
    }
}

/// Checks a name before asking Mojang: 3 to 16 letters, digits or `_`.
pub fn validate_name(name: &str) -> Result<(), String> {
    let n = name.chars().count();
    if n < 3 {
        return Err("Names have at least 3 characters.".into());
    }
    if n > 16 {
        return Err("Names have at most 16 characters.".into());
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_'))
    {
        return Err(format!(
            "Names use only letters, digits and _ (not \"{c}\")."
        ));
    }
    Ok(())
}

/// Checks that `bytes` are a skin Minecraft takes: a PNG of 64×64 or 64×32.
pub fn check_skin_png(bytes: &[u8]) -> anyhow::Result<(u32, u32)> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        bail!("That file isn't a PNG image. Skins are PNG files.");
    }
    if bytes.len() > 1 << 20 {
        bail!("That PNG is too large for a skin.");
    }
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .context("That PNG can't be read")?;
    let (w, h) = (img.width(), img.height());
    if (w, h) != (64, 64) && (w, h) != (64, 32) {
        bail!("Skins are 64×64 or 64×32 pixels; this image is {w}×{h}.");
    }
    Ok((w, h))
}

/// A `multipart/form-data` body for a skin upload: (content type, body).
pub fn multipart(variant: Variant, png: &[u8]) -> (String, Vec<u8>) {
    let boundary = format!(
        "----HopperSkin{}",
        &sha1_smol::Sha1::from(png).digest().to_string()[..16]
    );
    let mut body = Vec::with_capacity(png.len() + 400);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"variant\"\r\n\r\n{}\r\n",
            variant.api()
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"skin.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(png);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// A readable sentence for a failed call.
fn error_sentence(status: u16, body: &Value, doing: &str) -> String {
    let detail = body["details"]["status"].as_str().unwrap_or_default();
    let message = body["errorMessage"]
        .as_str()
        .or(body["error"].as_str())
        .unwrap_or_default();
    let why = match (status, detail) {
        (_, "DUPLICATE") => "someone already has that name".to_string(),
        (_, "NOT_ALLOWED") => "Mojang doesn't allow that name".to_string(),
        (401, _) => "the sign-in expired; sign in again".to_string(),
        (403, _) if doing.contains("name") => {
            "Mojang doesn't allow a name change yet (one every 30 days)".to_string()
        }
        (403, _) => "Mojang refused it for this account".to_string(),
        (404, _) => "this account doesn't own Minecraft: Java Edition".to_string(),
        (429, _) => "Mojang is rate limiting; wait a minute and try again".to_string(),
        (400, _) if !message.is_empty() => format!("Mojang said: {}", message.trim_end_matches('.')),
        (400, _) => "Mojang said the request was invalid".to_string(),
        (500.., _) => format!("Mojang's servers had a problem ({status}); try again later"),
        _ if !message.is_empty() => format!("{} ({status})", message.trim_end_matches('.')),
        _ => format!("Mojang answered {status}"),
    };
    format!("Couldn't {doing}: {why}.")
}

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

/// The Minecraft Services API for one signed-in player.
#[derive(Clone)]
pub struct Api {
    token: String,
}

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Api { .. }")
    }
}

type Response = ureq::http::Response<ureq::Body>;

impl Api {
    pub fn new(access_token: impl Into<String>) -> Self {
        Self { token: access_token.into() }
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// The body as JSON when the status is 2xx, else a readable error.
    fn answer(r: Result<Response, ureq::Error>, doing: &str) -> anyhow::Result<Value> {
        let mut r = r.map_err(|e| anyhow::anyhow!("Couldn't {doing}: {e}."))?;
        let status = r.status().as_u16();
        let text = r.body_mut().read_to_string().unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            bail!(error_sentence(status, &v, doing));
        }
        Ok(v)
    }

    fn profile_from(&self, v: Value) -> anyhow::Result<McProfile> {
        // Changes answer with the new profile; when one doesn't, read it.
        match serde_json::from_value::<McProfile>(v) {
            Ok(p) => Ok(p),
            Err(_) => self.profile(),
        }
    }

    pub fn profile(&self) -> anyhow::Result<McProfile> {
        let r = agent().get(API).header("Authorization", self.auth()).call();
        let v = Self::answer(r, "read your Minecraft profile")?;
        parse_profile(v)
    }

    /// Wears the skin at `url` (Mojang downloads it).
    pub fn set_skin_url(&self, variant: Variant, url: &str) -> anyhow::Result<McProfile> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            bail!("A skin URL starts with http:// or https://.");
        }
        let r = agent()
            .post(format!("{API}/skins"))
            .header("Authorization", self.auth())
            .send_json(json!({ "variant": variant.api(), "url": url }));
        self.profile_from(Self::answer(r, "change your skin")?)
    }

    /// Uploads a skin PNG (64×64 or 64×32).
    pub fn upload_skin(&self, variant: Variant, png: &[u8]) -> anyhow::Result<McProfile> {
        check_skin_png(png)?;
        let (content_type, body) = multipart(variant, png);
        let r = agent()
            .post(format!("{API}/skins"))
            .header("Authorization", self.auth())
            .header("Content-Type", content_type)
            .send(&body[..]);
        self.profile_from(Self::answer(r, "upload your skin")?)
    }

    /// Back to the default skin.
    pub fn reset_skin(&self) -> anyhow::Result<McProfile> {
        let r = agent()
            .delete(format!("{API}/skins/active"))
            .header("Authorization", self.auth())
            .call();
        self.profile_from(Self::answer(r, "reset your skin")?)
    }

    pub fn show_cape(&self, cape_id: &str) -> anyhow::Result<McProfile> {
        let r = agent()
            .put(format!("{API}/capes/active"))
            .header("Authorization", self.auth())
            .send_json(json!({ "capeId": cape_id }));
        self.profile_from(Self::answer(r, "change your cape")?)
    }

    pub fn hide_cape(&self) -> anyhow::Result<McProfile> {
        let r = agent()
            .delete(format!("{API}/capes/active"))
            .header("Authorization", self.auth())
            .call();
        self.profile_from(Self::answer(r, "hide your cape")?)
    }

    pub fn name_change(&self) -> anyhow::Result<NameChange> {
        let r = agent()
            .get(format!("{API}/namechange"))
            .header("Authorization", self.auth())
            .call();
        let v = Self::answer(r, "check when you can change your name")?;
        parse_name_change(v)
    }

    pub fn name_available(&self, name: &str) -> anyhow::Result<NameStatus> {
        validate_name(name).map_err(anyhow::Error::msg)?;
        let r = agent()
            .get(format!("{API}/name/{name}/available"))
            .header("Authorization", self.auth())
            .call();
        NameStatus::parse(&Self::answer(r, "check that name")?)
    }

    /// Changes the name. Mojang then locks it for 30 days.
    pub fn change_name(&self, name: &str) -> anyhow::Result<McProfile> {
        validate_name(name).map_err(anyhow::Error::msg)?;
        let r = agent()
            .put(format!("{API}/name/{name}"))
            .header("Authorization", self.auth())
            .send_empty();
        self.profile_from(Self::answer(r, "change your name")?)
    }
}

pub fn parse_profile(v: Value) -> anyhow::Result<McProfile> {
    serde_json::from_value(v).context("Mojang sent a profile Hopper can't read")
}

pub fn parse_name_change(v: Value) -> anyhow::Result<NameChange> {
    serde_json::from_value(v).context("Mojang sent name change details Hopper can't read")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = r#"{
        "id": "069a79f444e94726a5befca90e38aaf5",
        "name": "Notch",
        "skins": [
            {"id": "6a6e65e5-76dd-4c3c-a625-162924514568", "state": "ACTIVE",
             "url": "http://textures.minecraft.net/texture/292009a4925b58f02c77dadc3ecef07ea4c7472f64e0fdc32ce5522489362680",
             "textureKey": "292009a4925b58f02c77dadc3ecef07ea4c7472f64e0fdc32ce5522489362680",
             "variant": "SLIM"}
        ],
        "capes": [
            {"id": "1981aad3-73a4-4ef5-8d7d-4b0a1d5b1e4f", "state": "INACTIVE",
             "url": "http://textures.minecraft.net/texture/2340c0e03dd24a11b15a8b33c2a7e9e32abb2051b2481d0ba7defd635ca7a933",
             "alias": "Migrator"},
            {"id": "8f2f2fc3-0b7f-4bfa-8b4c-1a4b7f6c2b1e", "state": "ACTIVE",
             "url": "http://textures.minecraft.net/texture/afd553b39358a24edfe3b8a9a939fa5fa4faa4d9a9c3d6af8eafb377fa05c2bb",
             "alias": "Pan"}
        ],
        "profileActions": {}
    }"#;

    #[test]
    fn parses_a_profile() {
        let p = parse_profile(serde_json::from_str(PROFILE).unwrap()).unwrap();
        assert_eq!(p.name, "Notch");
        let skin = p.active_skin().unwrap();
        assert_eq!(skin.variant, Variant::Slim);
        assert!(skin.texture_key.starts_with("292009"));
        assert_eq!(p.capes.len(), 2);
        assert_eq!(p.active_cape().unwrap().alias, "Pan");
    }

    #[test]
    fn a_profile_without_skins_or_capes_still_parses() {
        let p = parse_profile(json!({"id": "a", "name": "Steve"})).unwrap();
        assert!(p.active_skin().is_none() && p.active_cape().is_none());
        assert!(parse_profile(json!({"name": 3})).is_err());
    }

    #[test]
    fn variants_ignore_case() {
        let s: Skin = serde_json::from_value(json!({"id":"x","url":"u","variant":"classic"})).unwrap();
        assert_eq!(s.variant, Variant::Classic);
        let s: Skin = serde_json::from_value(json!({"id":"x","url":"u","variant":"Slim"})).unwrap();
        assert_eq!(s.variant, Variant::Slim);
        let s: Skin = serde_json::from_value(json!({"id":"x","url":"u"})).unwrap();
        assert_eq!(s.variant, Variant::Classic);
    }

    #[test]
    fn name_change_cooldown() {
        let n = parse_name_change(json!({
            "changedAt": "2026-10-01T12:00:00Z",
            "createdAt": "2015-01-01T00:00:00Z",
            "nameChangeAllowed": false
        }))
        .unwrap();
        let next = n.next_change().unwrap();
        assert_eq!(crate::time::date(next), "31/10/2026");
        let now = crate::time::parse_iso("2026-10-13T12:00:00Z").unwrap();
        assert_eq!(n.describe(now), "You can change it again on 31/10/2026 (in 18 days).");
        let ok = parse_name_change(json!({"changedAt": null, "nameChangeAllowed": true})).unwrap();
        assert_eq!(ok.next_change(), None);
        assert_eq!(ok.describe(now), "You can change your name now.");
    }

    #[test]
    fn name_status() {
        assert_eq!(NameStatus::parse(&json!({"status":"AVAILABLE"})).unwrap(), NameStatus::Available);
        assert_eq!(NameStatus::parse(&json!({"status":"DUPLICATE"})).unwrap(), NameStatus::Taken);
        assert_eq!(NameStatus::parse(&json!({"status":"NOT_ALLOWED"})).unwrap(), NameStatus::NotAllowed);
        assert!(NameStatus::parse(&json!({})).is_err());
    }

    #[test]
    fn validates_names() {
        assert!(validate_name("McFawx").is_ok());
        assert!(validate_name("a_b_9").is_ok());
        assert!(validate_name("ab").unwrap_err().contains("at least 3"));
        assert!(validate_name("abcdefghijklmnopq").unwrap_err().contains("at most 16"));
        assert!(validate_name("bad name").unwrap_err().contains("\" \""));
        assert!(validate_name("naïve").is_err());
    }

    #[test]
    fn errors_are_sentences() {
        let e = error_sentence(400, &json!({"details":{"status":"DUPLICATE"}}), "change your name");
        assert_eq!(e, "Couldn't change your name: someone already has that name.");
        let e = error_sentence(429, &Value::Null, "upload your skin");
        assert_eq!(e, "Couldn't upload your skin: Mojang is rate limiting; wait a minute and try again.");
        let e = error_sentence(403, &Value::Null, "change your name");
        assert!(e.contains("30 days"));
        let e = error_sentence(400, &json!({"errorMessage":"Invalid skin."}), "upload your skin");
        assert_eq!(e, "Couldn't upload your skin: Mojang said: Invalid skin.");
        assert!(error_sentence(503, &Value::Null, "x").ends_with("try again later."));
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([1, 2, 3, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn checks_skin_pngs() {
        assert_eq!(check_skin_png(&png(64, 64)).unwrap(), (64, 64));
        assert_eq!(check_skin_png(&png(64, 32)).unwrap(), (64, 32));
        let e = check_skin_png(&png(128, 128)).unwrap_err().to_string();
        assert!(e.contains("128×128"), "{e}");
        assert!(check_skin_png(b"GIF89a....").unwrap_err().to_string().contains("isn't a PNG"));
    }

    #[test]
    fn builds_a_multipart_body() {
        let skin = png(64, 64);
        let (ct, body) = multipart(Variant::Slim, &skin);
        let boundary = ct.strip_prefix("multipart/form-data; boundary=").unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with(&format!("--{boundary}\r\n")));
        assert!(text.contains("name=\"variant\"\r\n\r\nslim\r\n"));
        assert!(text.contains("name=\"file\"; filename=\"skin.png\"\r\nContent-Type: image/png\r\n\r\n"));
        assert!(text.ends_with(&format!("\r\n--{boundary}--\r\n")));
        // The PNG is in there unchanged.
        assert!(body.windows(skin.len()).any(|w| w == skin.as_slice()));
    }

    #[test]
    fn api_debug_hides_the_token() {
        assert_eq!(format!("{:?}", Api::new("secret")), "Api { .. }");
    }
}
