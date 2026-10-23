//! Hopper Server (`hopperd`): a standalone panel for Minecraft servers, with
//! a web UI and the API Hopper desktop drives. Plan: design/server-panel.md.
//!
//! ```text
//! hopperd                         run the panel (port 8790 by default)
//! hopperd --servers-root <dir>    set the servers folder and run
//! hopperd --port <n>              set the port and run
//! hopperd --data <dir>            keep config and accounts in <dir>
//! hopperd --version
//! ```

mod api;
mod auth;
mod config;
mod discovery;
mod host;
mod servers;
mod ui;

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

pub struct App {
    pub cfg: RwLock<Config>,
    pub data_dir: PathBuf,
    pub auth: auth::Auth,
    pub servers: servers::Servers,
    pub host: host::Shared,
    /// Where the panel answers, e.g. "http://192.168.50.3:8790".
    pub addresses: Vec<String>,
    pub snapshot: watch::Sender<Arc<Snapshot>>,
    /// Wakes the snapshot loop early.
    pub refresh: Notify,
}

impl App {
    pub fn rescan(&self) {
        let cfg = self.cfg.read().unwrap().clone();
        self.servers.scan(cfg.servers_root.as_deref(), &cfg.servers);
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
fn absolute(p: &std::path::Path) -> PathBuf {
    let full = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = full.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => full,
    }
}

struct Args {
    data: Option<PathBuf>,
    port: Option<u16>,
    servers_root: Option<PathBuf>,
}

fn parse_args() -> anyhow::Result<Option<Args>> {
    let mut a = Args { data: None, port: None, servers_root: None };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--version" | "-V" => {
                println!("hopperd {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--data" => a.data = Some(val()?.into()),
            "--port" => a.port = Some(val()?.parse().context("--port takes a number")?),
            "--servers-root" => a.servers_root = Some(val()?.into()),
            "--help" | "-h" => {
                println!("{}", include_str!("main.rs").lines().skip(3).take(7).map(|l| l.trim_start_matches("//! ")).collect::<Vec<_>>().join("\n"));
                return Ok(None);
            }
            other => anyhow::bail!("Unknown argument {other}. Try --help."),
        }
    }
    Ok(Some(a))
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,mdns_sd=warn")).init();
    let Some(args) = parse_args()? else { return Ok(()) };
    let data_dir = args.data.unwrap_or_else(config::data_dir);
    std::fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let mut cfg = Config::load_or_create(&data_dir)?;
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

    let auth = auth::Auth::load(&data_dir)?;
    if let Some(code) = auth.setup_code() {
        let _ = std::fs::write(data_dir.join(SETUP_FILE), &code);
        log::warn!("No owner yet. Open the panel and use setup code {code} (also in {}).", data_dir.join(SETUP_FILE).display());
    }

    let lan = discovery::lan_addresses();
    let addresses = lan.iter().map(|ip| format!("http://{ip}:{}", cfg.port)).collect();
    let host = host::start(cfg.servers_root.clone());
    let (snapshot, _) = watch::channel(Arc::new(Snapshot::default()));
    let app = Arc::new(App {
        cfg: RwLock::new(cfg.clone()),
        data_dir: data_dir.clone(),
        auth,
        servers: servers::Servers::new(),
        host,
        addresses,
        snapshot,
        refresh: Notify::new(),
    });
    app.rescan();
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

    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(serve(app, cfg))
}

async fn serve(app: Arc<App>, cfg: Config) -> anyhow::Result<()> {
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

    let addr: SocketAddr = format!("{}:{}", cfg.bind, cfg.port).parse().context("bind address")?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("listening on {addr} (is another hopperd running?)"))?;
    log::info!("hopperd {} on {}", env!("CARGO_PKG_VERSION"), app.addresses.first().cloned().unwrap_or_else(|| format!("http://{addr}")));
    axum::serve(listener, api::router(app).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            log::info!("stopping the panel; servers keep running");
        })
        .await?;
    Ok(())
}
