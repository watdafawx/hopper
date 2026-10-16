//! Keys Hopper keeps for the user (the CurseForge and server panel API
//! keys), in Windows Credential Manager rather than in settings.json.

use anyhow::Context as _;

const SERVICE: &str = "Hopper";

/// The CurseForge API key.
pub const CF_KEY: &str = "curseforge_api_key";
/// The server panel's API key.
pub const PANEL_KEY: &str = "panel_api_key";

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
