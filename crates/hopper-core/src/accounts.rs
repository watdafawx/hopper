//! Several Microsoft-signed-in Minecraft accounts, one of them active (the
//! one games start with). The list (profile id, name, when last used) is a
//! small JSON file in Hopper's data folder; each account's refresh token is
//! in Windows Credential Manager, keyed by its profile id.

use crate::msauth::{self, Session};
use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    /// Minecraft profile id (no dashes).
    pub id: String,
    pub name: String,
    /// ISO time it was last made active or used to launch.
    pub last_used: Option<String>,
    /// The skin texture last seen, for its head in the list.
    pub skin_url: Option<String>,
    /// Copied from Hopper's first single sign-in.
    pub from_legacy: bool,
}

impl Saved {
    /// A head from a public skin service, until the skin is known.
    pub fn fallback_avatar(&self) -> String {
        format!("https://mc-heads.net/avatar/{}/32", self.id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Accounts {
    pub active: Option<String>,
    pub list: Vec<Saved>,
    /// The old single sign-in has been copied into the list (it is never
    /// copied twice, even after that account signs out).
    pub imported_legacy: bool,
}

impl Accounts {
    pub fn path() -> Option<PathBuf> {
        crate::paths::dirs().map(|d| d.data_dir().join("accounts.json"))
    }

    pub fn load() -> Self {
        Self::path().map(|p| Self::load_from(&p)).unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path().context("no data folder")?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.part");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn active(&self) -> Option<&Saved> {
        let id = self.active.as_deref()?;
        self.list.iter().find(|a| a.id == id)
    }

    pub fn get(&self, id: &str) -> Option<&Saved> {
        self.list.iter().find(|a| a.id == id)
    }

    /// Adds the account or refreshes its name, and makes it active.
    pub fn upsert_active(&mut self, id: &str, name: &str) {
        let now = crate::time::now_iso();
        match self.list.iter_mut().find(|a| a.id == id) {
            Some(a) => {
                a.name = name.to_string();
                a.last_used = Some(now);
            }
            None => self.list.push(Saved {
                id: id.to_string(),
                name: name.to_string(),
                last_used: Some(now),
                ..Default::default()
            }),
        }
        self.active = Some(id.to_string());
    }

    /// Makes `id` active. False when there is no such account.
    pub fn set_active(&mut self, id: &str) -> bool {
        let Some(a) = self.list.iter_mut().find(|a| a.id == id) else {
            return false;
        };
        a.last_used = Some(crate::time::now_iso());
        self.active = Some(id.to_string());
        true
    }

    /// Takes `id` out of the list; the most recently used one left becomes
    /// active when it was. Returns the removed account.
    pub fn remove(&mut self, id: &str) -> Option<Saved> {
        let ix = self.list.iter().position(|a| a.id == id)?;
        let gone = self.list.remove(ix);
        if self.active.as_deref() == Some(id) {
            self.active = self
                .list
                .iter()
                .max_by(|a, b| a.last_used.cmp(&b.last_used))
                .map(|a| a.id.clone());
        }
        Some(gone)
    }

    /// Remembers what a profile looks like now.
    pub fn note_profile(&mut self, id: &str, name: &str, skin_url: Option<&str>) -> bool {
        let Some(a) = self.list.iter_mut().find(|a| a.id == id) else {
            return false;
        };
        let before = (a.name.clone(), a.skin_url.clone());
        a.name = name.to_string();
        if let Some(u) = skin_url {
            a.skin_url = Some(u.to_string());
        }
        before != (a.name.clone(), a.skin_url.clone())
    }
}

// ── the file, guarded so two threads don't interleave changes ───────

static FILE: Mutex<()> = Mutex::new(());

/// Loads the list, changes it and saves it, holding the lock.
pub fn edit<T>(f: impl FnOnce(&mut Accounts) -> T) -> anyhow::Result<T> {
    let _g = FILE.lock().unwrap_or_else(|e| e.into_inner());
    let mut a = Accounts::load();
    let before = a.clone();
    let out = f(&mut a);
    if a != before {
        a.save()?;
    }
    Ok(out)
}

// ── tokens ──────────────────────────────────────────────────────────

fn get_token(id: &str) -> Option<String> {
    msauth::account_entry(id).ok()?.get_password().ok()
}

fn save_token(id: &str, token: &str) -> anyhow::Result<()> {
    msauth::account_entry(id)?
        .set_password(token)
        .context("Couldn't save the sign-in in Windows Credential Manager")
}

fn delete_token(id: &str) -> anyhow::Result<()> {
    match msauth::account_entry(id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Sessions already made this run. Minecraft tokens last a day; one is
/// reused for a few hours so profile changes don't sign in every time.
static SESSIONS: Mutex<Option<HashMap<String, (Instant, Session)>>> = Mutex::new(None);
const SESSION_REUSE: Duration = Duration::from_secs(4 * 3600);

fn cached(id: &str) -> Option<Session> {
    let g = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    let (at, s) = g.as_ref()?.get(id)?;
    (at.elapsed() < SESSION_REUSE).then(|| s.clone())
}

fn remember(s: &Session) {
    let mut g = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new)
        .insert(s.profile.id.clone(), (Instant::now(), s.clone()));
}

fn forget_session(id: &str) {
    let mut g = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = g.as_mut() {
        m.remove(id);
    }
}

// ── what the app calls ──────────────────────────────────────────────

/// Whether games can start with a Microsoft account: an active one with a
/// saved token, or the old single sign-in still to be copied.
pub fn has_login() -> bool {
    let a = Accounts::load();
    match a.active() {
        Some(s) => get_token(&s.id).is_some(),
        None => !a.imported_legacy && msauth::legacy_refresh().is_some(),
    }
}

/// Whether the old single sign-in is waiting to be copied into the list.
pub fn legacy_pending() -> bool {
    let a = Accounts::load();
    a.active().is_none() && !a.imported_legacy && msauth::legacy_refresh().is_some()
}

/// A session for account `id`, signing in with its saved token when there
/// is no recent one (the rotated token is saved).
pub fn session(id: &str) -> anyhow::Result<Session> {
    if let Some(s) = cached(id) {
        return Ok(s);
    }
    let token = get_token(id).context("This account isn't signed in any more; add it again.")?;
    let (token, session) = msauth::session_from_refresh(&token)?;
    save_token(id, &token)?;
    remember(&session);
    if session.profile.id != id {
        bail!("Microsoft signed in a different Minecraft profile than expected.");
    }
    let name = session.profile.name.clone();
    edit(|a| a.note_profile(id, &name, None))?;
    Ok(session)
}

/// A session for the active account, copying the old single sign-in into
/// the list first if that hasn't happened yet. The old entry stays.
pub fn active_session() -> anyhow::Result<Session> {
    let a = Accounts::load();
    if let Some(s) = a.active() {
        let id = s.id.clone();
        let session = session(&id)?;
        edit(|a| a.set_active(&id))?;
        return Ok(session);
    }
    if !a.imported_legacy
        && let Some(old) = msauth::legacy_refresh()
    {
        return import_legacy(&old);
    }
    bail!("Not signed in with Microsoft yet; add an account first.")
}

/// Copies the old single sign-in into the account list (non-destructive:
/// the old Credential Manager entry is left as it is).
fn import_legacy(old: &str) -> anyhow::Result<Session> {
    let (token, session) = msauth::session_from_refresh(old)?;
    let id = session.profile.id.clone();
    save_token(&id, &token)?;
    remember(&session);
    let name = session.profile.name.clone();
    edit(|a| {
        a.upsert_active(&id, &name);
        if let Some(s) = a.list.iter_mut().find(|s| s.id == id) {
            s.from_legacy = true;
        }
        a.imported_legacy = true;
    })?;
    Ok(session)
}

/// Saves a new sign-in (from the device code flow) and makes it active.
pub fn add(refresh: &str, session: &Session) -> anyhow::Result<()> {
    let id = session.profile.id.clone();
    save_token(&id, refresh)?;
    remember(session);
    let name = session.profile.name.clone();
    edit(|a| {
        // Adding the account the old sign-in held counts as copying it.
        if a.active().is_none() && msauth::legacy_refresh().is_some() {
            a.imported_legacy = true;
        }
        a.upsert_active(&id, &name);
    })?;
    Ok(())
}

/// Makes `id` the account games start with.
pub fn switch(id: &str) -> anyhow::Result<()> {
    if !edit(|a| a.set_active(id))? {
        bail!("That account isn't in the list any more.");
    }
    Ok(())
}

/// Signs `id` out of Hopper: its token and list entry go. When it came from
/// the old single sign-in, that entry goes too, so the token isn't left
/// behind.
pub fn sign_out(id: &str) -> anyhow::Result<()> {
    delete_token(id)?;
    forget_session(id);
    let gone = edit(|a| a.remove(id))?;
    if gone.is_some_and(|g| g.from_legacy) {
        msauth::delete_legacy()?;
    }
    Ok(())
}

/// Notes the profile's current name and skin for the list.
pub fn note_profile(id: &str, name: &str, skin_url: Option<&str>) -> anyhow::Result<()> {
    edit(|a| a.note_profile(id, name, skin_url))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_round_trips_and_switches() {
        let dir = std::env::temp_dir().join(format!("hopper-accts-{}", std::process::id()));
        let path = dir.join("accounts.json");
        let mut a = Accounts::default();
        a.upsert_active("id1", "Steve");
        a.upsert_active("id2", "Alex");
        assert_eq!(a.active().unwrap().name, "Alex");
        assert!(a.set_active("id1"));
        assert!(!a.set_active("nope"));
        a.save_to(&path).unwrap();
        let b = Accounts::load_from(&path);
        assert_eq!(a, b);
        assert_eq!(b.active().unwrap().name, "Steve");
        // The file has no secrets in it, just ids, names and times.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"Alex\"") && !text.contains("token"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn removing_the_active_one_picks_the_latest_other() {
        let mut a = Accounts::default();
        for (id, t) in [("a", "2026-01-01"), ("b", "2026-03-01"), ("c", "2026-02-01")] {
            a.list.push(Saved {
                id: id.into(),
                name: id.into(),
                last_used: Some(t.into()),
                ..Default::default()
            });
        }
        a.active = Some("a".into());
        assert_eq!(a.remove("a").unwrap().id, "a");
        assert_eq!(a.active.as_deref(), Some("b"));
        a.remove("c");
        assert_eq!(a.active.as_deref(), Some("b"), "removing another keeps the active one");
        a.remove("b");
        assert_eq!(a.active, None);
        assert!(a.remove("b").is_none());
    }

    #[test]
    fn notes_profile_changes() {
        let mut a = Accounts::default();
        a.upsert_active("x", "Old");
        assert!(a.note_profile("x", "New", Some("http://t/1")));
        assert!(!a.note_profile("x", "New", None), "nothing changed");
        assert_eq!(a.get("x").unwrap().skin_url.as_deref(), Some("http://t/1"));
        assert!(!a.note_profile("y", "Who", None));
    }

    #[test]
    fn missing_or_broken_files_load_empty() {
        let dir = std::env::temp_dir().join(format!("hopper-accts-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(Accounts::load_from(&dir.join("none.json")), Accounts::default());
        std::fs::write(dir.join("bad.json"), "{nope").unwrap();
        assert_eq!(Accounts::load_from(&dir.join("bad.json")), Accounts::default());
        std::fs::remove_dir_all(dir).ok();
    }
}
