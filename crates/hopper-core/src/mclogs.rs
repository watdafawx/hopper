//! Sharing a log on mclo.gs, the paste service modded Minecraft uses for
//! crash logs. Ported from mod-sync: one POST returns a short link. Home
//! folder names are scrubbed first unless the user turns that off.

use regex::Regex;
use std::sync::LazyLock;

const MAX_BYTES: usize = 10 << 20;

static WIN_USER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)([A-Z]:[\\/]+Users[\\/]+)[^\\/\r\n"']+"#).unwrap());
static NIX_HOME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(/home/|/Users/)[^/\r\n"']+"#).unwrap());

/// Replaces the user folder in paths with `<user>`, and the player name
/// when given.
pub fn scrub(text: &str, player: Option<&str>) -> String {
    let s = WIN_USER.replace_all(text, "${1}<user>");
    let mut s = NIX_HOME.replace_all(&s, "${1}<user>").into_owned();
    if let Some(p) = player.filter(|p| p.len() >= 3) {
        s = s.replace(p, "<player>");
    }
    s
}

/// Uploads and returns the share URL.
pub fn upload(text: &str) -> anyhow::Result<String> {
    let text = if text.len() > MAX_BYTES {
        // The crash is at the end; keep the tail.
        let cut = text.ceil_char_boundary(text.len() - MAX_BYTES + 64);
        format!("...[start trimmed for upload]...\n{}", &text[cut..])
    } else {
        text.to_string()
    };
    let mut r = ureq::post("https://api.mclo.gs/1/log").send_form([("content", text.as_str())])?;
    let v: serde_json::Value = r.body_mut().read_json()?;
    parse(&v)
}

fn parse(v: &serde_json::Value) -> anyhow::Result<String> {
    if v["success"].as_bool() == Some(true)
        && let Some(url) = v["url"].as_str()
    {
        return Ok(url.to_string());
    }
    anyhow::bail!(
        "mclo.gs rejected the log: {}",
        v["error"].as_str().unwrap_or("no reason given")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_user_folders_and_player() {
        let s = scrub(
            r"at C:\Users\MIcha\AppData and G:/Users/someone/x, /home/bob/.minecraft, Setting user: McFawx",
            Some("McFawx"),
        );
        assert_eq!(
            s,
            r"at C:\Users\<user>\AppData and G:/Users/<user>/x, /home/<user>/.minecraft, Setting user: <player>"
        );
    }

    #[test]
    fn reads_the_reply() {
        let ok = serde_json::json!({"success": true, "id": "abc", "url": "https://mclo.gs/abc"});
        assert_eq!(parse(&ok).unwrap(), "https://mclo.gs/abc");
        let bad = serde_json::json!({"success": false, "error": "Required POST argument 'content' is empty."});
        assert!(parse(&bad).unwrap_err().to_string().contains("empty"));
    }
}
