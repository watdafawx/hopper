//! Hopper Server (`hopperd`): a standalone panel for Minecraft servers, with
//! a web UI and the API Hopper desktop drives. Plan: design/server-panel.md.
//!
//! ```text
//! hopperd                         run the panel (port 8790 by default)
//! hopperd --portable              the same (Windows: no first-run dialog)
//! hopperd --servers-root <dir>    set the servers folder and run
//! hopperd --port <n>              set the port and run
//! hopperd --bind <addr>           listen on <addr> (0.0.0.0 = every network)
//! hopperd --data <dir>            keep config and accounts in <dir>
//! hopperd service install         run in the background with the machine
//! hopperd service uninstall|status
//! hopperd --version
//! ```
//!
//! On Windows, double-clicking hopperd.exe asks how to run it (just run it,
//! start with Windows, or install as a service) and runs a tray icon: see
//! `launcher`.

mod acme;
mod alerts;
mod api;
mod auth;
mod changes;
mod autoupdate;
mod updswap;
mod backups_api;
mod browse_api;
mod config;
mod config_sync;
mod configs;
mod control;
mod discovery;
mod files;
mod fs_api;
mod fs_zip;
mod grants;
mod home;
mod host;
#[cfg(windows)]
mod launcher;
mod logs_api;
mod manage_api;
mod mcp;
mod mods_api;
mod mods_hunt;
mod mods_more;
mod mods_upload;
mod mods_table;
mod motd_live;
mod moves;
mod secrets;
mod security;
mod relay;
#[cfg(test)]
mod relay_tests;
#[cfg(test)]
mod moves_tests;
mod net;
mod offsite;
mod pack_api;
mod parity_browse;
mod parity_console;
mod parity_files;
mod parity_mods;
mod parity_overview;
mod parity_worlds;
mod player_ping;
mod packs_where_api;
mod hold;
mod limbo;
mod router;
mod rules;
mod servers;
mod server_ops;
mod servers_admin;
mod service;
mod sftp;
mod sources_api;
mod tls;
mod transfer;
mod nodes;
#[cfg(test)]
mod nodes_tests;
#[cfg(test)]
mod pair_tests;
mod tunnel;

#[cfg(windows)]
mod tray;
mod ui;
mod web_access;
mod web_panels;
#[cfg(test)]
mod cross_origin_tests;
#[cfg(test)]
mod secrets_tests;
mod webhooks;
mod worlds_api;
mod zipstream;

use anyhow::Context as _;
use config::Config;
use serde::Serialize;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{Notify, watch};

pub const SETUP_FILE: &str = "setup-code.txt";

/// What every client sees live: all servers and the host, refreshed once a
/// second (and right after a power action).
#[derive(Clone, Default, Serialize)]
pub struct Snapshot {
    pub servers: Vec<servers::Summary>,
    pub host: host::HostStats,
}

/// What clients need to trust the panel's certificate.
pub struct TlsInfo {
    /// SHA-256 of the panel's CA, "AB:CD:…".
    pub fingerprint: String,
    pub ca_pem: String,
}

pub struct App {
    pub cfg: RwLock<Config>,
    pub data_dir: PathBuf,
    /// The folder hopperd starts from (`--data`); with a panel folder it
    /// only points there.
    pub boot_dir: PathBuf,
    pub auth: auth::Auth,
    pub servers: servers::Servers,
    pub host: host::Shared,
    /// Where the panel answers, e.g. "https://192.168.50.3:8790" (kept
    /// current as the machine's addresses change).
    pub addresses: RwLock<Vec<String>>,
    pub snapshot: watch::Sender<Arc<Snapshot>>,
    /// Wakes the snapshot loop early.
    pub refresh: Notify,
    /// Long work (server pack updates) Hopper desktop follows.
    pub jobs: hopper_core::jobs::Jobs,
    /// Runs schedules: restarts, crash restarts, backups, autostart.
    pub scheduler: std::sync::Mutex<Option<hopper_core::hosting::Scheduler>>,
    pub limiter: security::Limiter,
    pub audit: security::Audit,
    pub tls: TlsInfo,
    /// Let's Encrypt certificates (see [`acme`]).
    pub acme: Arc<acme::Acme>,
    /// The SFTP host key's "SHA256:…".
    pub sftp_fingerprint: Option<String>,
    /// Server addresses (the Minecraft port shared by every server).
    pub router: router::Router,
    /// Finished jobs' results, until fetched.
    pub results: std::sync::Mutex<std::collections::HashMap<u64, serde_json::Value>>,
    /// Which server each job is for (`pack_api::JobServers`).
    pub job_servers: pack_api::JobServers,
    /// What changed, for every open client (see [`changes`]).
    pub changes: changes::Changes,
    /// One-use WebSocket tickets and file links for pages on other origins.
    pub grants: grants::Grants,
    /// Nodes of this panel (see [`nodes`]).
    pub nodes: nodes::Nodes,
    /// Servers being moved or copied between this panel and its nodes.
    pub moves: moves::Moves,
}

impl App {
    pub fn rescan(&self) {
        let cfg = self.cfg.read().unwrap().clone();
        self.servers.scan_except(cfg.servers_root.as_deref(), &cfg.servers, &cfg.hidden);
    }

    fn take_snapshot(&self) -> Snapshot {
        let mut eff = router::effective(self);
        Snapshot {
            servers: self.servers.list().iter().map(|e| router::fill_summary(self.servers.summary(e), &mut eff)).collect(),
            host: self.host.read().unwrap().clone(),
        }
    }
}

/// `p` as a full path, without Windows' `\\?\` prefix (canonicalize adds
/// it; java, run.bat and people don't want it).
pub(crate) fn absolute(p: &std::path::Path) -> PathBuf {
    let full = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = full.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => full,
    }
}

struct Args {
    /// Started by the Windows service manager.
    service: bool,
    data: Option<PathBuf>,
    port: Option<u16>,
    servers_root: Option<PathBuf>,
    /// The panel folder (normally picked at setup).
    home: Option<PathBuf>,
    /// Address to listen on (`--bind`).
    bind: Option<String>,
}

fn parse_args() -> anyhow::Result<Option<Args>> {
    let mut a = Args { service: false, data: None, port: None, servers_root: None, home: None, bind: None };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--version" | "-V" => {
                println!("hopperd {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--data" => a.data = Some(val()?.into()),
            "--service" => a.service = true,
            // Run as today, without the first-run dialog.
            "--portable" => {}
            "--port" => a.port = Some(val()?.parse().context("--port takes a number")?),
            "--servers-root" => a.servers_root = Some(val()?.into()),
            "--home" => a.home = Some(val()?.into()),
            "--bind" => a.bind = Some(val()?),
            "--help" | "-h" => {
                println!("{}", include_str!("main.rs").lines().skip(4).take(9).map(|l| l.trim_start_matches("//! ")).collect::<Vec<_>>().join("\n"));
                return Ok(None);
            }
            other => anyhow::bail!("Unknown argument {other}. Try --help."),
        }
    }
    Ok(Some(a))
}

type Shutdown = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// The folder this run keeps its config in: `--data`, else the default.
/// Read straight from the command line, before [`parse_args`] has run.
fn data_dir_from_args() -> PathBuf {
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        if arg == "--data" {
            if let Some(v) = it.next() {
                return PathBuf::from(v);
            }
        }
    }
    config::data_dir()
}

/// The panel's logger: env_logger's usual line, with secrets hidden the
/// same way Hopper desktop's log hides them (`hopper_core::applog::redact`).
/// A log ends up in bug reports, the systemd journal and `docker logs`, so a
/// token, join key, cookie or URL with a key in it must never be in one.
pub(crate) fn logger(default: &str) -> env_logger::Builder {
    use std::io::Write as _;
    let mut b = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default));
    b.format(|buf, r| {
        let msg = r.args().to_string();
        writeln!(buf, "[{} {:<5} {}] {}", buf.timestamp(), r.level(), r.target(), hopper_core::applog::redact(&msg))
    });
    b
}

/// The setup code goes to a person at the console (a terminal, or `docker
/// logs`, which is how a Docker install is set up), never into a log file
/// or the systemd journal: there the line points at `setup-code.txt`, which
/// only the panel's user can read. `JOURNAL_STREAM` is how systemd says
/// stderr is the journal.
fn tell_setup_code(code: &str, port: u16, local_ok: bool, file: &std::path::Path) {
    use std::io::IsTerminal as _;
    let to_console = !autoupdate::AS_SERVICE.load(std::sync::atomic::Ordering::SeqCst)
        && std::env::var_os("JOURNAL_STREAM").is_none()
        && (std::io::stderr().is_terminal() || auth::in_docker());
    let local = if local_ok { format!(" On this PC, open http://localhost:{port} to set the owner password without it.") } else { String::new() };
    if to_console {
        eprintln!("No owner yet. Open the panel and use setup code {code}.{local} (Also in {}.)", file.display());
    }
    log::warn!("No owner yet. Open the panel and use the setup code in {}.{local}", file.display());
}

/// The data folder holds the accounts (password hashes, two-factor
/// secrets), the setup code, API and node token hashes and the config's
/// keys, so on Linux and macOS only the panel's own user may open it. Each
/// file is written with the default mode (readable by everyone under the
/// usual umask); closing the folder covers all of them at once, including
/// files from older versions. Windows folders under the user's profile are
/// already the user's only.
fn private_dir(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let Ok(m) = std::fs::metadata(dir) else { return };
        if m.permissions().mode() & 0o077 != 0 {
            match std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
                Ok(()) => log::info!("{} is now readable by this user only", dir.display()),
                Err(e) => log::warn!("couldn't make {} private: {e}", dir.display()),
            }
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn main() -> anyhow::Result<()> {
    // Before anything else, so a panic leaves a crash log beside hopperd.log.
    hopper_core::panic_log::install("Hopper Server", env!("CARGO_PKG_VERSION"), &data_dir_from_args());
    let _ = rustls::crypto::ring::default_provider().install_default();
    control::init();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Double-clicked (not from a terminal): the first-run dialog and tray.
    #[cfg(windows)]
    if argv.is_empty() && launcher::double_clicked() {
        return launcher::start();
    }
    #[cfg(windows)]
    if argv.first().map(String::as_str) == Some("--tray") {
        return launcher::tray_only();
    }
    let mut log = logger("info,mdns_sd=warn");
    if argv.first().map(String::as_str) == Some("service") {
        log.init();
        return service::command(&argv[1..]);
    }
    // Node mode: join a main panel, check or leave it (design/multi-panel.md, A1).
    if argv.first().map(String::as_str) == Some("node") {
        log.filter_level(log::LevelFilter::Warn).init();
        return tunnel::node::command(&argv[1..]);
    }
    let Some(args) = parse_args()? else { return Ok(()) };
    if args.service {
        // No console as a service: log to hopperd.log in the data folder.
        let dir = args.data.clone().unwrap_or_else(config::data_dir);
        let _ = std::fs::create_dir_all(&dir);
        let file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("hopperd.log"))?;
        log.target(env_logger::Target::Pipe(Box::new(file))).init();
        autoupdate::AS_SERVICE.store(true, std::sync::atomic::Ordering::SeqCst);
        #[cfg(windows)]
        return service::run_as_service();
        #[cfg(not(windows))]
        anyhow::bail!("--service is for the Windows service manager. Use hopperd service install.");
    }
    log.init();
    let r = run_main(Box::pin(async {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = control::quitting() => {}
        }
    }));
    control::withdraw();
    match control::exit_code() {
        0 => r,
        code => std::process::exit(code),
    }
}

/// Runs the panel until `shutdown` finishes.
pub(crate) fn run_main(shutdown: Shutdown) -> anyhow::Result<()> {
    let Some(args) = parse_args()? else { return Ok(()) };
    let boot_dir = args.data.unwrap_or_else(config::data_dir);
    std::fs::create_dir_all(&boot_dir).with_context(|| format!("creating {}", boot_dir.display()))?;
    // Linux: the servers' console FIFOs, in the panel's own folder so a
    // restarted panel finds them again (before any server is loaded).
    hopper_core::hosting::process::set_stdin_dir(boot_dir.join("run"));
    let mut cfg = Config::load_or_create(&boot_dir)?;
    if let Some(h) = &args.home {
        let h = absolute(h);
        std::fs::create_dir_all(&h).with_context(|| format!("creating {}", h.display()))?;
        if cfg.home.as_ref() != Some(&h) {
            cfg.home = Some(h);
            cfg.save(&boot_dir)?;
        }
    }
    // With a panel folder, the panel's data lives in it (moved in the
    // first time); the start-up folder only points there.
    let (data_dir, loaded) = home::settle(&boot_dir, cfg)?;
    let mut cfg = loaded;
    private_dir(&boot_dir);
    private_dir(&data_dir);
    mods_api::keep_updates_in(&data_dir);
    servers_admin::purge_old(cfg.servers_root.as_deref());
    transfer::purge_stale(cfg.servers_root.as_deref());
    transfer::purge_old_plans(&data_dir);
    let mut changed = false;
    if let Some(p) = args.port {
        cfg.port = p;
        changed = true;
    }
    if let Some(r) = args.servers_root {
        cfg.servers_root = Some(absolute(&r));
        changed = true;
    }
    if let Some(b) = &args.bind {
        cfg.bind = b.clone();
        changed = true;
    }
    if changed {
        cfg.save(&data_dir)?;
    }
    // A scratch copy (--home, or HOPPER_LOOPBACK=1) listens on this PC only
    // unless --bind says otherwise: Windows asks for firewall access for
    // every new program that listens on all networks. Not saved, so the
    // config's own address still counts for a real run.
    let scratch = args.home.is_some() && hopper_core::paths::loopback_env() != Some(false) || hopper_core::paths::loopback_env() == Some(true);
    if args.bind.is_none() && scratch && matches!(cfg.bind.as_str(), "0.0.0.0" | "::" | "") {
        cfg.bind = "127.0.0.1".into();
        cfg.discovery = false;
        log::info!("scratch run: listening on 127.0.0.1 only (use --bind to change)");
    }

    if !cfg.curseforge_key.trim().is_empty() {
        hopper_core::cf::set_session_key(&cfg.curseforge_key);
    }
    // The panel keeps its GitHub token in its config, never the system's
    // credential store.
    hopper_core::sources::http::set_session_github_token(&cfg.github_token);
    hopper_core::sources::set_enabled(cfg.allow_sources);
    let auth = auth::Auth::load(&data_dir)?;
    // Sessions of pages taken off the list (hopperd.toml edited by hand) end.
    auth.end_remote_sessions_except(&cfg.web_origins);
    if let Some(code) = auth.setup_code() {
        let _ = std::fs::write(data_dir.join(SETUP_FILE), &code);
        let file = data_dir.join(SETUP_FILE);
        tell_setup_code(&code, cfg.port, auth::local_setup_possible(cfg.trust_local_setup), &file);
    }

    let ca = tls::load_or_create_ca(&data_dir, &cfg.name)?;
    let tls_info = TlsInfo { fingerprint: tls::fingerprint(&ca.der), ca_pem: ca.pem.clone() };
    let net_now = NetIdentity::now(&cfg);
    let lan = net_now.lan.clone();
    let own_cert: tls::OwnCert = Arc::new(RwLock::new(Arc::new(tls::issue(&ca, &net_now.names, &net_now.ips)?)));
    let addresses = net_now.addresses.clone();
    log::info!("certificate fingerprint {}", tls_info.fingerprint);
    let host = host::start(cfg.servers_root.clone());
    let (snapshot, _) = watch::channel(Arc::new(Snapshot::default()));
    let app = Arc::new(App {
        cfg: RwLock::new(cfg.clone()),
        data_dir: data_dir.clone(),
        boot_dir: boot_dir.clone(),
        auth,
        servers: servers::Servers::new(),
        host,
        addresses: RwLock::new(addresses),
        snapshot,
        refresh: Notify::new(),
        jobs: Default::default(),
        results: Default::default(),
        job_servers: Default::default(),
        changes: Default::default(),
        grants: Default::default(),
        router: Default::default(),
        scheduler: Default::default(),
        limiter: Default::default(),
        audit: security::Audit::new(&data_dir),
        tls: tls_info,
        acme: acme::Acme::new(&data_dir),
        sftp_fingerprint: sftp::fingerprint(&data_dir),
        nodes: nodes::Nodes::new(&data_dir),
        moves: moves::Moves::new(&data_dir),
    });
    app.rescan();
    autoupdate::start(app.clone());
    // Old backups go to the server's trash, not away for good. At start
    // nothing goes: Mods › Backups shows what Clean up would remove.
    hopper_core::retention::set_policy(cfg.backup_retention);
    hopper_core::retention::set_disposer(mods_more::to_server_trash);
    // Read every server's mod jars once in the background; the Mods tab
    // then opens from the scan cache instead of opening hundreds of jars.
    {
        let roots: Vec<PathBuf> = app.servers.list().iter().map(|e| e.root.join("mods")).filter(|d| d.is_dir()).collect();
        std::thread::Builder::new().name("warm-mod-scan".into()).spawn(move || {
            for d in roots {
                let t = std::time::Instant::now();
                match hopper_core::scan::scan_folder(&d) {
                    Ok(m) => log::info!("read {} mods in {} ({:.1}s)", m.len(), d.display(), t.elapsed().as_secs_f32()),
                    Err(e) => log::warn!("reading {}: {e:#}", d.display()),
                }
            }
        }).ok();
    }
    // Schedules: daily restarts, crash restarts, backups; and servers set
    // to start with the panel.
    crate::mods_hunt::resume_note(&app);
    let sched = hopper_core::hosting::Scheduler::start(app.servers.panel());
    sched.start_autostart(&app.servers.panel());
    *app.scheduler.lock().unwrap() = Some(sched);
    log::info!(
        "{} server(s) in {}",
        app.servers.list().len(),
        cfg.servers_root.as_ref().map_or("(no servers folder set: use --servers-root)".into(), |p| p.display().to_string())
    );

    let _mdns = if cfg.discovery {
        match discovery::announce(&cfg.name, cfg.port, &lan) {
            Ok(d) => Some(d),
            Err(e) => {
                log::warn!("LAN discovery off: {e:#}");
                None
            }
        }
    } else {
        None
    };

    keep_addresses_current(app.clone(), ca, own_cert.clone(), net_now);

    // Old "changes in the files" results, and any jar a scan left in the temp folder.
    std::thread::spawn(hopper_core::changelog_files::tidy);
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(serve(app, cfg, own_cert, shutdown))
}

/// This machine's names and addresses, and the panel links they make.
#[derive(PartialEq)]
struct NetIdentity {
    lan: Vec<std::net::IpAddr>,
    names: Vec<String>,
    ips: Vec<std::net::IpAddr>,
    addresses: Vec<String>,
}

impl NetIdentity {
    fn now(cfg: &Config) -> Self {
        let lan = discovery::lan_addresses();
        let names = tls::local_names(&cfg.public_names);
        let mut ips = lan.clone();
        ips.push([127, 0, 0, 1].into());
        ips.extend(cfg.public_names.iter().filter_map(|n| n.parse::<std::net::IpAddr>().ok()));
        let mut addresses: Vec<String> = lan.iter().map(|ip| format!("https://{ip}:{}", cfg.port)).collect();
        if let Some(ts) = names.iter().find(|n| n.ends_with(".ts.net")) {
            addresses.push(format!("https://{ts}:{}", cfg.port));
        }
        for n in &cfg.public_names {
            addresses.push(format!("https://{n}:{}", cfg.port));
        }
        if let Some(d) = &cfg.acme_domain {
            addresses.push(format!("https://{d}"));
        }
        NetIdentity { lan, names, ips, addresses }
    }
}

/// Re-issues the panel's certificate and its address list when the
/// machine's addresses change. As a service, hopperd often starts before
/// the network card has its address; without this the certificate would
/// never name the LAN IP and clients connecting by IP are refused.
fn keep_addresses_current(app: Arc<App>, ca: tls::Ca, own: tls::OwnCert, mut last: NetIdentity) {
    let _ = std::thread::Builder::new().name("addresses".into()).spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(20));
            let cfg = app.cfg.read().unwrap().clone();
            let now = NetIdentity::now(&cfg);
            if now == last {
                continue;
            }
            match tls::issue(&ca, &now.names, &now.ips) {
                Ok(cert) => {
                    *own.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(cert);
                    *app.addresses.write().unwrap() = now.addresses.clone();
                    log::info!("addresses changed: certificate now names {:?} and {:?}", now.names, now.ips);
                    last = now;
                }
                Err(e) => log::warn!("re-issuing the certificate: {e:#}"),
            }
        }
    });
}

async fn serve(app: Arc<App>, cfg: Config, own_cert: tls::OwnCert, shutdown: Shutdown) -> anyhow::Result<()> {
    // The live snapshot: once a second, or right away when asked.
    let a = app.clone();
    tokio::spawn(async move {
        loop {
            let b = a.clone();
            if let Ok(snap) = tokio::task::spawn_blocking(move || b.take_snapshot()).await {
                a.snapshot.send_replace(Arc::new(snap));
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = a.refresh.notified() => {
                    // Let the action take effect first.
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            }
        }
    });

    webhooks::watch(app.clone());
    changes::watch(app.clone());
    // A permanent delete that stopped half way leaves .hopper-deleting-*.
    if let Some(root) = cfg.servers_root.clone() {
        let _ = std::thread::Builder::new().name("sweep-deleting".into()).spawn(move || {
            let n = hopper_core::hosting::fsmove::sweep_deleting(&root);
            if n > 0 {
                log::info!("cleaned up {n} folder(s) left over from deleting servers");
            }
        });
    }
    rules::start(app.clone());
    router::start(app.clone());
    // As a main: alerts for nodes that are gone. As a node: the
    // connection to the main, while node.json is there.
    nodes::start(app.clone());
    tunnel::node::start(app.clone());
    if cfg.sftp_port != 0 {
        tokio::spawn(sftp::serve(app.clone(), cfg.bind.clone(), cfg.sftp_port));
    }

    let addr: SocketAddr = format!("{}:{}", cfg.bind, cfg.port).parse().context("bind address")?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("listening on {addr} (is another hopperd running?)"))?;
    control::announce(cfg.port);
    // Let's Encrypt, when automatic certificates are on.
    acme::start(app.clone());
    let resolver = |internet| tls::Resolver { own: own_cert.clone(), acme: app.acme.clone(), internet };
    let tls = net::Tls { lan: tls::server_config(resolver(false)), internet: tls::server_config(resolver(true)) };

    let first = app.addresses.read().unwrap().first().cloned();
    log::info!("hopperd {} on {}", env!("CARGO_PKG_VERSION"), first.unwrap_or_else(|| format!("https://{addr}")));
    net::serve(listener, api::router(app), tls, async {
        shutdown.await;
        log::info!("stopping the panel; servers keep running");
    })
    .await;
    Ok(())
}

/// The data folder holds password hashes, sign-in secrets and keys: on
/// Linux it ends up readable by its own user only.
#[cfg(all(test, unix))]
#[test]
fn the_data_folder_is_made_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("hopperd-private-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    private_dir(&dir);
    assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    let _ = std::fs::remove_dir_all(&dir);
}
