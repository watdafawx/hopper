//! Keys Hopper keeps for the user (the CurseForge and server panel API
//! keys), in Windows Credential Manager rather than in settings.json.

use anyhow::Context as _;

const SERVICE: &str = "Hopper";

/// The CurseForge API key.
pub const CF_KEY: &str = "curseforge_api_key";
/// The server panel's API key.
pub const PANEL_KEY: &str = "panel_api_key";
/// The token a Hopper Server gave this PC when it was paired.
pub const HOPPERD_KEY: &str = "hopperd_token";

/// The token key of a paired Hopper Server (`id` "" is the first one).
pub fn hopperd_key(id: &str) -> String {
    if id.is_empty() { HOPPERD_KEY.to_string() } else { format!("{HOPPERD_KEY}.{id}") }
}

/// The API key name of Crafty / MCSS / Pterodactyl panel `id` ("" is the
/// one set up before several panels were possible).
pub fn panel_key(id: &str) -> String {
    if id.is_empty() { PANEL_KEY.to_string() } else { format!("{PANEL_KEY}.{id}") }
}

fn entry(name: &str) -> anyhow::Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, name).context("Credential Manager")
}

/// The saved secret, if any.
pub fn get(name: &str) -> Option<String> {
    entry(name).ok()?.get_password().ok()
}

/// Saves a secret; empty removes it.
pub fn set(name: &str, value: &str) -> anyhow::Result<()> {
    let e = entry(name)?;
    if value.trim().is_empty() {
        return match e.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        };
    }
    e.set_password(value.trim())?;
    Ok(())
}
