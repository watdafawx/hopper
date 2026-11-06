//! A server on a paired Hopper Server made from an instance on this PC: the
//! same content a server on this PC gets ([`super::create_from_instance`]),
//! sent over as one zip the panel builds from, or the pack's official server
//! pack, which the panel downloads itself.

use super::create::{CreateReport, server_content_zip};
use crate::hopperd::Client;
use crate::instances::Instance;
use crate::jobs::Job;
use anyhow::Context as _;
use serde_json::{Value, json};
use std::collections::HashSet;

/// What to make, as the New server dialog has it.
#[derive(Clone, Debug, Default)]
pub struct PanelServer {
    pub name: String,
    pub motd: String,
    pub memory_mb: u32,
    /// None: the panel picks a free one.
    pub port: Option<u16>,
    pub address: String,
    /// Build from the pack's official server pack when it has one.
    pub server_pack: bool,
    /// Start it once it's made.
    pub start: bool,
}

/// What [`create_from_instance`] did.
#[derive(Clone, Debug, Default)]
pub struct PanelReport {
    /// The panel's answer: the new server's `id` and `path`.
    pub made: Value,
    /// What went into the zip; None when the panel used the server pack.
    pub content: Option<CreateReport>,
}

impl PanelReport {
    /// The new server's folder name on the panel.
    pub fn folder(&self) -> Option<String> {
        self.made["path"]
            .as_str()
            .and_then(|p| p.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next())
            .filter(|f| !f.is_empty())
            .map(str::to_string)
    }
}

/// Makes a server for `inst` on the panel `client` talks to, following the
/// panel's job in `job`. With `o.server_pack` and a CurseForge pack that has
/// one, the panel builds from the official server pack; otherwise the
/// instance's server-side mods and config go over in a zip (client-only mods
/// left out, as on this PC). The job's notes say what was copied and left out.
pub fn create_from_instance(
    client: &Client,
    inst: &Instance,
    o: &PanelServer,
    sync_anyway: &HashSet<u64>,
    hidden: &HashSet<String>,
    job: &Job,
) -> anyhow::Result<PanelReport> {
    let common = json!({
        "name": o.name,
        "memory_mb": o.memory_mb,
        "port": o.port,
        "address": o.address.trim(),
        "accept_eula": true,
        "start": o.start,
    });
    let with = |extra: Value| {
        let mut b = common.clone();
        if let (Some(b), Some(e)) = (b.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        b
    };

    if o.server_pack
        && super::has_server_pack(inst)
        && let Some(p) = &inst.pack
    {
        job.detail(format!("Asking the panel to build from {}'s server pack", p.name));
        let made = client.run_job(
            "/servers/create-from-pack",
            with(json!({ "source": "curseforge", "project": p.project_id.to_string(), "file": p.file_id.to_string() })),
            job,
        )?;
        set_motd(client, &made, &o.motd, &o.name);
        job.note(format!(
            "Built from {}'s server pack. Mods you added to the instance yourself go over on the server's Sync tab.",
            if p.name.is_empty() { &inst.name } else { &p.name }
        ));
        return Ok(PanelReport { made, content: None });
    }

    let dir = std::env::temp_dir().join(format!("hopper-panel-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let zip = dir.join(format!("{}.zip", Instance::folder_name(&o.name)));
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(zip.clone());
    let report = server_content_zip(inst, sync_anyway, hidden, &zip, job)?;

    let made = if report.mods_copied == 0 {
        // The panel's zip import wants mods; with none to send there is
        // only the loader to make.
        job.detail("Making the server on the panel");
        let (kind, version) = super::ServerLoader::from_instance(&inst.loader)?.panel_kind();
        client.run_job(
            "/servers/create",
            with(json!({ "mc_version": inst.game_version, "loader": kind, "loader_version": version, "motd": o.motd })),
            job,
        )?
    } else {
        let size = std::fs::metadata(&zip).map(|m| m.len()).unwrap_or(0);
        let mb = (size as f64 / 1_048_576.0).max(0.1);
        let upload = client
            .upload_incoming(&zip, |sent| {
                job.detail(format!("Sending the server files ({mb:.0} MB)"));
                job.progress((sent * 1000 / size.max(1)) as usize, 1000);
            })
            .context("sending the server files to the panel")?;
        job.check_cancelled()?;
        let made = client.run_job(
            "/servers/import-zip",
            with(json!({ "upload": upload, "mc_version": inst.game_version, "loader": inst.loader })),
            job,
        )?;
        set_motd(client, &made, &o.motd, &o.name);
        made
    };
    job.note(format!(
        "Copied {} mods and {} config files; left out {} client-only.",
        report.mods_copied,
        report.files_copied,
        report.skipped.len()
    ));
    if !report.skipped.is_empty() {
        let names: Vec<&str> = report.skipped.iter().take(8).map(|(f, _)| f.as_str()).collect();
        let more = report.skipped.len().saturating_sub(names.len());
        job.note(format!(
            "Left out: {}{}. The full list is in left-out-client-mods.txt in the server folder.",
            names.join(", "),
            if more > 0 { format!(" and {more} more") } else { String::new() }
        ));
    }
    if let Some(n) = report.note.clone() {
        job.note(n);
    }
    for n in report.missing_notes() {
        job.note(n);
    }
    Ok(PanelReport { made, content: Some(report) })
}

/// Sets the new server's MOTD when it isn't the name (which the panel uses).
pub fn set_motd(client: &Client, made: &Value, motd: &str, name: &str) {
    let motd = motd.trim();
    let Some(id) = made["id"].as_str() else { return };
    if motd.is_empty() || motd == name.trim() {
        return;
    }
    if let Err(e) = client.put(&Client::server_path(id, "/properties"), json!({ "values": { "motd": motd } })) {
        log::warn!("setting the new server's MOTD: {e:#}");
    }
}
