//! The Minecraft account the launcher signs in with, for display only.
//! Only the profile name and id are read; tokens in the file are ignored.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct Account {
    pub name: String,
    pub uuid: String,
}

impl Account {
    /// Face image for the account (public skin service).
    pub fn avatar(&self) -> String {
        format!("https://mc-heads.net/avatar/{}/32", self.uuid)
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct File {
    accounts: HashMap<String, Entry>,
    active_account_local_id: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Entry {
    minecraft_profile: Option<Profile>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Profile {
    id: String,
    name: String,
}

pub fn active(launcher_dir: &Path) -> Option<Account> {
    let f: File =
        serde_json::from_slice(&std::fs::read(launcher_dir.join("launcher_accounts.json")).ok()?)
            .ok()?;
    let p = f
        .accounts
        .get(&f.active_account_local_id)?
        .minecraft_profile
        .as_ref()?;
    (!p.name.is_empty()).then(|| Account {
        name: p.name.clone(),
        uuid: p.id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_active_profile_only() {
        let dir = std::env::temp_dir().join(format!("hopper-acct-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("launcher_accounts.json"),
            r#"{"accounts":{"a":{"accessToken":"secret","minecraftProfile":{"id":"u1","name":"Steve"}},"b":{"minecraftProfile":{"id":"u2","name":"Alex"}}},"activeAccountLocalId":"a"}"#,
        )
        .unwrap();
        let a = active(&dir).unwrap();
        assert_eq!((a.name.as_str(), a.uuid.as_str()), ("Steve", "u1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
