fn main() {
    embed_cf_key();
    embed_ms_client_id();
    embed_changelog();
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    if !windows {
        return;
    }
    // The logo as the exe's icon (resource 1): Explorer, the taskbar and
    // GPUI's window icon all read it.
    println!("cargo:rerun-if-changed=assets/hopper.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon_with_id("assets/hopper.ico", "1");
    if let Err(e) = res.compile() {
        println!("cargo:warning=no exe icon: {e}");
    }
    // GPUI builds deep element trees on the main thread; Windows gives it
    // 1 MB by default, which a debug build can outgrow.
    println!("cargo:rustc-link-arg-bin=hopper=/STACK:16777216");
}

/// Builds the Azure app id for Microsoft sign-in (MS_CLIENT_ID, from the
/// environment, e.g. a CI secret, or .env) into the exe, so released copies
/// can sign in and launch. A client id isn't a secret: every launcher ships
/// its own.
fn embed_ms_client_id() {
    println!("cargo:rerun-if-env-changed=MS_CLIENT_ID");
    println!("cargo:rerun-if-changed=.env");
    let from_file = || {
        std::fs::read_to_string(".env").ok()?.lines().find_map(|l| {
            let v = l.trim().strip_prefix("MS_CLIENT_ID=")?;
            Some(v.trim().trim_matches('"').to_string())
        })
    };
    if let Some(id) = std::env::var("MS_CLIENT_ID").ok().filter(|s| !s.trim().is_empty()).or_else(from_file)
        && !id.trim().is_empty()
    {
        println!("cargo:rustc-env=HOPPER_MS_CLIENT_ID={}", id.trim());
    }
}

/// With HOPPER_EMBED_CF_KEY=1, builds the CurseForge key (CF_API_KEY, from
/// the environment or .env) into the exe, so a copy works without setup.
/// Off by default: anyone with the exe can read a built-in key.
fn embed_cf_key() {
    println!("cargo:rerun-if-env-changed=HOPPER_EMBED_CF_KEY");
    println!("cargo:rerun-if-env-changed=CF_API_KEY");
    if std::env::var("HOPPER_EMBED_CF_KEY").as_deref() != Ok("1") {
        return;
    }
    println!("cargo:rerun-if-changed=.env");
    let from_file = || {
        std::fs::read_to_string(".env").ok()?.lines().find_map(|l| {
            let v = l.trim().strip_prefix("CF_API_KEY=")?;
            Some(v.trim().trim_matches('"').to_string())
        })
    };
    match std::env::var("CF_API_KEY").ok().or_else(from_file) {
        Some(k) if !k.is_empty() => println!("cargo:rustc-env=HOPPER_CF_KEY={k}"),
        _ => println!("cargo:warning=HOPPER_EMBED_CF_KEY is set but no CF_API_KEY was found"),
    }
}

/// Builds the notes of the last releases into the exe for What's new,
/// newest first, as `version \x1f notes` records split by `\x1e`. A
/// release's notes are its "Hopper x.y.z" commit's body, then the subjects
/// of the commits it shipped (those since the release before). Without git
/// (or history) the list is empty.
fn embed_changelog() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/logs/HEAD");
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("changelog.txt");
    let log = std::process::Command::new("git")
        .args(["log", "-n", "600", "--format=%s%x1f%b%x1e"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    // (version, paragraphs), newest first; commits before the newest
    // release aren't out yet.
    let mut releases: Vec<(String, Vec<String>)> = Vec::new();
    for rec in log.split('\x1e') {
        let Some((subject, body)) = rec.trim_start().split_once('\x1f') else { continue };
        let subject = subject.trim();
        let version = subject
            .strip_prefix("Hopper ")
            .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.'));
        if let Some(version) = version {
            if releases.len() == 60 {
                break;
            }
            let body: Vec<&str> = body
                .lines()
                .filter(|l| !l.trim_start().to_lowercase().starts_with("co-authored-by:"))
                .collect();
            let body = body.join("\n").trim().to_string();
            releases.push((version.to_string(), if body.is_empty() { vec![] } else { vec![body] }));
        } else if let Some((_, notes)) = releases.last_mut()
            && !subject.is_empty()
            && !subject.starts_with("Cargo.lock")
            && !subject.starts_with("Merge ")
        {
            notes.push(subject.to_string());
        }
    }
    let records: Vec<String> = releases.into_iter().map(|(v, n)| format!("{v}\x1f{}", n.join("\n\n"))).collect();
    std::fs::write(out, records.join("\x1e")).unwrap();
}
