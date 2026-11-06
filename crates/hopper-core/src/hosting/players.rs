//! A server's players as the panel API shows and changes them: the ops,
//! whitelist and ban lists from the server's own json files, and changes
//! sent as the server's own commands (so it looks names up and writes the
//! files itself). Hopper Server and desktop's servers on this PC share it.

use anyhow::bail;
use serde_json::{Value, json};
use std::path::Path;

/// The players in one of the server's lists (`ops.json`, `whitelist.json`,
/// `banned-players.json`): name, uuid, reason, level.
pub fn names_in(root: &Path, file: &str) -> Vec<Value> {
    std::fs::read(root.join(file))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| {
            let name = v["name"].as_str()?.to_string();
            Some(json!({ "name": name, "uuid": v["uuid"], "reason": v["reason"], "level": v["level"] }))
        })
        .collect()
}

/// The lists together, as `GET /servers/{id}/players` sends them (without
/// who's online).
pub fn lists_json(root: &Path) -> Value {
    let props = super::Properties::load(&root.join("server.properties")).unwrap_or_default();
    json!({
        "ops": names_in(root, "ops.json"),
        "whitelist": names_in(root, "whitelist.json"),
        "banned": names_in(root, "banned-players.json"),
        "whitelist_on": props.get("white-list").is_some_and(|v| v.trim() == "true"),
    })
}

/// Player name rules: up to 16 letters, digits, underscores.
pub fn valid_name(n: &str) -> bool {
    (1..=16).contains(&n.len()) && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The server command for a Players tab action (op, kick, ban, ...).
pub fn command_for(action: &str, name: &str, reason: &str) -> anyhow::Result<String> {
    let name = name.trim();
    if !valid_name(name) {
        bail!("\"{name}\" isn't a Minecraft name.");
    }
    let reason = reason.replace(['\n', '\r'], " ");
    let cmd = match action {
        "op" => format!("op {name}"),
        "deop" => format!("deop {name}"),
        "kick" => format!("kick {name} {reason}"),
        "ban" => format!("ban {name} {reason}"),
        "pardon" => format!("pardon {name}"),
        "whitelist_add" => format!("whitelist add {name}"),
        "whitelist_remove" => format!("whitelist remove {name}"),
        other => bail!("Unknown action {other}"),
    };
    Ok(cmd.trim().to_string())
}

/// Who's online from a `list` command's answer: "There are 2 of a max of
/// 20 players online: Alex, Steve" (older servers: "2/20 players online:").
pub fn parse_list(reply: &str) -> Vec<String> {
    let Some((_, names)) = reply.split_once("online:") else { return Vec::new() };
    names
        .split([',', '\n'])
        .map(|n| n.trim())
        .filter(|n| valid_name(n))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands() {
        assert_eq!(command_for("op", " Alex ", "").unwrap(), "op Alex");
        assert_eq!(command_for("kick", "Alex", "too\nloud").unwrap(), "kick Alex too loud");
        assert_eq!(command_for("whitelist_add", "jeb_", "").unwrap(), "whitelist add jeb_");
        assert!(command_for("op", "Alex; stop", "").is_err());
        assert!(command_for("explode", "Alex", "").is_err());
    }

    #[test]
    fn list_replies() {
        assert_eq!(parse_list("There are 2 of a max of 20 players online: Alex, Steve"), ["Alex", "Steve"]);
        assert!(parse_list("There are 0 of a max of 20 players online: ").is_empty());
        assert_eq!(parse_list("There are 1/10 players online:\nNotch"), ["Notch"]);
        assert!(parse_list("Unknown command").is_empty());
    }

    #[test]
    fn lists_from_files() {
        let root = std::env::temp_dir().join(format!("hopper-players-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("ops.json"), r#"[{"uuid":"u1","name":"Alex","level":4}]"#).unwrap();
        std::fs::write(root.join("server.properties"), "white-list=true\n").unwrap();
        let v = lists_json(&root);
        assert_eq!(v["ops"][0]["name"], "Alex");
        assert_eq!(v["ops"][0]["level"], 4);
        assert_eq!(v["whitelist"].as_array().unwrap().len(), 0);
        assert_eq!(v["whitelist_on"], true);
        let _ = std::fs::remove_dir_all(&root);
    }
}
