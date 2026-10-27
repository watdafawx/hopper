//! Hopper Server (`hopperd`): a standalone panel for Minecraft servers, with
//! a web UI and the API Hopper desktop drives. Plan: design/server-panel.md.
//!
//! ```text
//! hopperd                         run the panel (port 8790 by default)
//! hopperd --portable              the same (Windows: no first-run dialog)
//! hopperd --servers-root <dir>    set the servers folder and run
//! hopperd --port <n>              set the port and run
//! hopperd --data <dir>            keep config and accounts in <dir>
//! hopperd service install         run in the background with the machine
//! hopperd service uninstall|status
//! hopperd --version
//! ```
//!
//! On Windows, double-clicking hopperd.exe asks how to run it (just run it,
//! start with Windows, or install as a service) and runs a tray icon: see
//! `launcher`.

mod alerts;
mod api;
mod auth;
mod backups_api;
mod browse_api;
mod config;
mod control;
mod discovery;
mod files;
mod fs_api;
mod fs_zip;
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
mod mods_table;
mod security;
mod net;
mod offsite;
mod pack_api;
mod router;
mod servers;
mod server_ops;
mod servers_admin;
mod service;
mod sftp;
mod tls;
#[cfg(windows)]
mod tray;
mod ui;
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
    /// The SFTP host key's "SHA256:…".
    pub sftp_fingerprint: Option<String>,
    /// Server addresses (the Minecraft port shared by every server).
    pub router: router::Router,
    /// Finished jobs' results, until fetched.
    pub results: std::sync::Mutex<std::collections::HashMap<u64, serde_json::Value>>,
}

impl App {
    pub fn rescan(&self) {
        let cfg = self.cfg.read().unwrap().clone();
        self.servers.scan_except(cfg.servers_root.as_deref(), &cfg.servers, &cfg.hidden);
    }

    fn take_snapshot(&self) -> Snapshot {
        Snapshot {
            servers: self.servers.list().iter().map(|e| self.servers.summary(e)).collect(),
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
}

fn parse_args() -> anyhow::Result<Option<Args>> {
    let mut a = Args { service: false, data: None, port: None, servers_root: None, home: None };
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
            "--help" | "-h" => {
                println!("{}", include_str!("main.rs").lines().skip(4).take(8).map(|l| l.trim_start_matches("//! ")).collect::<Vec<_>>().join("\n"));
                return Ok(None);
            }
            other => anyhow::bail!("Unknown argument {other}. Try --help."),
        }
    }
    Ok(Some(a))
}

type Shutdown = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

fn main() -> anyhow::Result<()> {
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
    let mut log = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,mdns_sd=warn"));
    if argv.first().map(String::as_str) == Some("service") {
        log.init();
        return service::command(&argv[1..]);
    }
    let Some(args) = parse_args()? else { return Ok(()) };
    if args.service {
        // No console as a service: log to hopperd.log in the data folder.
        let dir = args.data.clone().unwrap_or_else(config::data_dir);
        let _ = std::fs::create_dir_all(&dir);
        let file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("hopperd.log"))?;
        log.target(env_logger::Target::Pipe(Box::new(file))).init();
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
    mods_api::keep_updates_in(&data_dir);
    servers_admin::purge_old(cfg.servers_root.as_deref());
    let mut changed = false;
    if let Some(p) = args.port {
        cfg.port = p;
        changed = true;
    }
    if let Some(r) = args.servers_root {
        cfg.servers_root = Some(absolute(&r));
        changed = true;
    }
    if changed {
        cfg.save(&data_dir)?;
    }

    if !cfg.curseforge_key.trim().is_empty() {
        hopper_core::cf::set_session_key(&cfg.curseforge_key);
    }
    let auth = auth::Auth::load(&data_dir)?;
    if let Some(code) = auth.setup_code() {
        let _ = std::fs::write(data_dir.join(SETUP_FILE), &code);
        let file = data_dir.join(SETUP_FILE);
        if auth::local_setup_possible(cfg.trust_local_setup) {
            log::warn!(
                "No owner yet. On this PC, open http://localhost:{} to set the owner password. From another device use setup code {code} (also in {}).",
                cfg.port,
                file.display()
            );
        } else {
            log::warn!("No owner yet. Open the panel and use setup code {code} (also in {}).", file.display());
        }
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
        router: Default::default(),
        scheduler: Default::default(),
        limiter: Default::default(),
        audit: security::Audit::new(&data_dir),
        tls: tls_info,
        sftp_fingerprint: sftp::fingerprint(&data_dir),
    });
    app.rescan();
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
    router::start(app.clone());
    if cfg.sftp_port != 0 {
        tokio::spawn(sftp::serve(app.clone(), cfg.bind.clone(), cfg.sftp_port));
    }

    let addr: SocketAddr = format!("{}:{}", cfg.bind, cfg.port).parse().context("bind address")?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("listening on {addr} (is another hopperd running?)"))?;
    control::announce(cfg.port);
    // Let's Encrypt, when a domain is set.
    let acme = cfg.acme_domain.clone().filter(|d| !d.trim().is_empty()).map(|domain| {
        use futures_util::StreamExt as _;
        let mut state = rustls_acme::AcmeConfig::new([domain.clone()])
            .contact(cfg.acme_email.trim().is_empty().then(Vec::<String>::new).unwrap_or_else(|| vec![format!("mailto:{}", cfg.acme_email.trim())]))
            .cache(rustls_acme::caches::DirCache::new(app.data_dir.join("acme")))
            .directory_lets_encrypt(!cfg.acme_staging)
            .state();
        let resolver = state.resolver();
        tokio::spawn(async move {
            while let Some(r) = state.next().await {
                match r {
                    Ok(ok) => log::info!("Let's Encrypt: {ok:?}"),
                    Err(e) => log::warn!("Let's Encrypt: {e:?}"),
                }
            }
        });
        log::info!("getting a Let's Encrypt certificate for {domain} (port 443 on it must reach this panel)");
        (domain, resolver)
    });
    let tls = tls::server_config(tls::Resolver { own: own_cert, acme });

    let first = app.addresses.read().unwrap().first().cloned();
    log::info!("hopperd {} on {}", env!("CARGO_PKG_VERSION"), first.unwrap_or_else(|| format!("https://{addr}")));
    net::serve(listener, api::router(app), tls, async {
        shutdown.await;
        log::info!("stopping the panel; servers keep running");
    })
    .await;
    Ok(())
}
