//! The lobby island's connection: a player who joins a server that's
//! starting waits on a small island with the start's progress in a boss
//! bar, and is moved to the server (a transfer) once it's up. Packets:
//! hopper_core::hosting::limbo.

use crate::App;
use crate::servers::Entry;
use hopper_core::hosting::limbo::{self as lb, BarColor};
use hopper_core::hosting::router::{self as core, Read};
use hopper_core::hosting::{Properties, ServerState};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// How a stay on the island ended.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Moved to the server.
    Moved,
    /// The start failed; the player was told.
    Failed,
    /// The client left, or wouldn't take the lobby (it was told to join
    /// again instead, or left during setup: see [`Outcome::Refused`]).
    Left,
    /// The client turned the lobby down (a modded client, or one without
    /// Minecraft's own data): it gets the "join again" message from now on.
    Refused(String),
    /// A Forge / NeoForge client on a modded server (named by its brand): it
    /// can't take the island, and is held at login next time if the server
    /// is set to wait for modded players.
    Modded(String),
}

/// Reads packets from a connection, keeping what's left over.
struct Packets {
    buf: Vec<u8>,
}

impl Packets {
    async fn next(&mut self, sock: &mut TcpStream, wait: Duration) -> anyhow::Result<(i32, Vec<u8>)> {
        let mut chunk = [0u8; 4096];
        loop {
            if let Read::Done((id, body), used) = core::read_packet(&self.buf, 1 << 20)? {
                let out = (id, body.to_vec());
                self.buf.drain(..used);
                return Ok(out);
            }
            let n = tokio::time::timeout(wait, sock.read(&mut chunk)).await??;
            if n == 0 {
                anyhow::bail!("closed");
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Reads until packet `want`, passing others to `seen`.
    async fn until(&mut self, sock: &mut TcpStream, want: i32, mut seen: impl FnMut(i32, &[u8])) -> anyhow::Result<Vec<u8>> {
        let start = Instant::now();
        loop {
            let left = Duration::from_secs(20).saturating_sub(start.elapsed());
            let (id, body) = self.next(sock, left.max(Duration::from_millis(1))).await?;
            if id == want {
                return Ok(body);
            }
            seen(id, &body);
        }
    }
}

/// Turns on `accepts-transfers` in the server's server.properties, which
/// the lobby's move needs; false if it couldn't.
pub fn allow_transfers(e: &Entry) -> bool {
    let path = e.root.join("server.properties");
    let mut p = Properties::load(&path).unwrap_or_default();
    if p.get("accepts-transfers").is_some_and(|v| v.trim() == "true") {
        return true;
    }
    p.set("accepts-transfers", "true");
    match p.save(&path) {
        Ok(()) => {
            log::info!("{}: turned on accepts-transfers in server.properties for the lobby island", e.name);
            true
        }
        Err(err) => {
            log::warn!("{}: couldn't turn on accepts-transfers: {err:#}", e.name);
            false
        }
    }
}

/// Whether the server takes transfers: its server.properties says so.
pub fn takes_transfers(e: &Entry) -> bool {
    Properties::load(&e.root.join("server.properties")).ok().and_then(|p| p.get("accepts-transfers").map(|v| v.trim() == "true")).unwrap_or(false)
}

fn bar_title(name: &str, e: &Entry) -> (Value, f32) {
    match e.server.boot_status() {
        Some(b) => {
            let left = match b.left_secs {
                _ if b.slow => ", taking longer than usual".to_string(),
                Some(l) => format!(", about {} left", hopper_core::hosting::boot::short_time(l)),
                None => String::new(),
            };
            (json!(format!("Starting {name}, {}%{left}", b.percent)), b.percent as f32 / 100.0)
        }
        None => (json!(format!("Starting {name}…")), 0.02),
    }
}

/// Runs the island for a player who just sent Login Start (protocol 767),
/// until the server is up (then a transfer to `host:port`), the start
/// fails, or the player leaves.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    app: &Arc<App>,
    sock: &mut TcpStream,
    leftover: Vec<u8>,
    e: &Arc<Entry>,
    player: &str,
    uuid: Option<[u8; 16]>,
    host: &str,
    port: u16,
    modded: bool,
) -> anyhow::Result<Outcome> {
    let name = e.name.clone();
    let rejoin = || core::standin_kick(core::Standin::Starting, &name, e.server.usual_start_secs(), e.server.boot_status().as_ref());
    let mut rx = Packets { buf: leftover };
    let id = uuid.filter(|u| u != &[0; 16]).unwrap_or_else(|| lb::offline_uuid(player));
    sock.write_all(&lb::login_success(id, player)).await?;
    rx.until(sock, lb::LOGIN_ACK, |_, _| {}).await?;

    // Configuration: Minecraft's own data, the registries, done.
    sock.write_all(&lb::known_packs()).await?;
    let mut brand = String::new();
    let packs = rx
        .until(sock, lb::CONFIG_IN_KNOWN_PACKS, |id, body| {
            if id == lb::CONFIG_IN_PLUGIN
                && let Some(b) = lb::brand(body)
            {
                brand = b;
            }
        })
        .await?;
    // A modded client on a modded server expects that server's mods,
    // which the island doesn't have: straight to the message.
    let forge = brand.to_ascii_lowercase().contains("forge");
    if forge && modded {
        sock.write_all(&lb::config_disconnect(&rejoin())).await?;
        return Ok(Outcome::Modded(format!("{brand} client")));
    }
    if !lb::has_core(&packs)? {
        sock.write_all(&lb::config_disconnect(&rejoin())).await?;
        return Ok(Outcome::Refused("no built-in data for 1.21.1".into()));
    }
    for p in lb::registry_packets() {
        sock.write_all(&p).await?;
    }
    sock.write_all(&lb::finish_configuration()).await?;
    if let Err(err) = rx.until(sock, lb::CONFIG_IN_FINISH, |_, _| {}).await {
        // Gone during setup: it didn't take the island's registries.
        return Ok(Outcome::Refused(format!("left while setting up ({err:#}, brand {brand:?})")));
    }

    // Play: the island, the bar, a line in chat.
    sock.write_all(&lb::play_login()).await?;
    sock.write_all(&lb::time_packet()).await?;
    for p in lb::spawn_packets() {
        sock.write_all(&p).await?;
    }
    for p in lb::chunk_packets() {
        sock.write_all(&p).await?;
    }
    let mut teleport = 1;
    sock.write_all(&lb::position_packet(teleport)).await?;
    sock.write_all(&lb::wait_for_chunks()).await?;
    let (title, progress) = bar_title(&name, e);
    sock.write_all(&lb::bar_add(&title, progress, BarColor::Blue)).await?;
    sock.write_all(&lb::chat(&json!({ "text": format!("{name} is starting. You'll move over when it's up."), "color": "yellow" }))).await?;
    sock.flush().await?;
    log::info!("router: {player} waits on the lobby island for {name} (client {brand:?})");

    let began = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut alive_at = Instant::now();
    let mut chunk = [0u8; 4096];
    loop {
        tokio::select! {
            r = sock.read(&mut chunk) => {
                let n = r?;
                if n == 0 {
                    return Ok(Outcome::Left);
                }
                rx.buf.extend_from_slice(&chunk[..n]);
                while let Read::Done((id, body), used) = core::read_packet(&rx.buf, 1 << 20)? {
                    // Fell off (past the rim, somehow): back to the spawn.
                    if lb::reported_y(id, body).is_some_and(|y| y < 50.0) {
                        teleport += 1;
                        sock.write_all(&lb::position_packet(teleport)).await?;
                    }
                    rx.buf.drain(..used);
                }
            }
            _ = tick.tick() => {
                let state = e.server.state();
                if state == ServerState::Running && up(e).await {
                    sock.write_all(&lb::bar_update(&json!(format!("{name} is up")), 1.0, BarColor::Green).concat()).await?;
                    sock.write_all(&lb::chat(&json!({ "text": format!("{name} is up! Moving you over…"), "color": "green" }))).await?;
                    sock.write_all(&lb::transfer(host, port)).await?;
                    sock.flush().await?;
                    log::info!("router: moved {player} from the lobby island to {name}");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    return Ok(Outcome::Moved);
                }
                let gone = matches!(state, ServerState::Stopped | ServerState::Crashed(_)) && !app.router.is_waking(&e.id);
                if gone {
                    sock.write_all(&lb::bar_update(&json!({ "text": "Failed to start", "color": "red" }), 1.0, BarColor::Red).concat()).await?;
                    sock.flush().await?;
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    let retry = crate::router::settings(e).retry_in_minutes(crate::router::unix_now());
                    sock.write_all(&lb::play_disconnect(&core::wake_failed_kick(&name, retry))).await?;
                    sock.flush().await?;
                    return Ok(Outcome::Failed);
                }
                if began.elapsed() > Duration::from_secs(15 * 60) {
                    sock.write_all(&lb::play_disconnect(&json!({ "text": format!("{name} is taking very long to start. Join again later.") }))).await?;
                    return Ok(Outcome::Left);
                }
                let (title, progress) = bar_title(&name, e);
                let color = if e.server.boot_status().is_some_and(|b| b.slow) { BarColor::Yellow } else { BarColor::Blue };
                sock.write_all(&lb::bar_update(&title, progress, color).concat()).await?;
                if alive_at.elapsed() >= Duration::from_secs(10) {
                    sock.write_all(&lb::keep_alive(began.elapsed().as_millis() as i64)).await?;
                    alive_at = Instant::now();
                }
            }
        }
    }
}

/// Whether the server answers on its port yet.
pub(crate) async fn up(e: &Entry) -> bool {
    let port = crate::router::port_of(e);
    matches!(tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(("127.0.0.1", port))).await, Ok(Ok(_)))
}
