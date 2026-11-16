//! The edge with a fake hopperd on the real local channel, a fake
//! Minecraft server and fake players: splices, hopperd going away and
//! coming back, fallback answers, and the channel's checks.

use super::local;
use super::proto::{self, Answer, Frame, MAX_FRAME, Msg, PROTO};
use super::serve::{Edge, RESTART_MOTD};
use hopper_core::hosting::router as core;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};

fn channel_name() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\hopperd-edge-test-{}-{n}", std::process::id())
    }
    #[cfg(unix)]
    {
        let d = std::env::temp_dir().join(format!("hopperd-edge-test-{}-{n}", std::process::id()));
        d.join("edge.sock").to_string_lossy().into_owned()
    }
}

async fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port()
}

/// A running edge on its own channel and player port.
struct Rig {
    channel: String,
    token: String,
    port: u16,
    edge: Arc<Edge>,
}

async fn rig(hold: Duration) -> Rig {
    let channel = channel_name();
    let token = "t".repeat(64);
    let edge = Edge::with_hold(token.clone(), hold);
    let l = local::Listener::bind(&channel).unwrap();
    let e = edge.clone();
    tokio::spawn(async move { e.run(l).await });
    Rig { channel, token, port: free_port().await, edge }
}

/// A fake hopperd on the channel.
struct Ctl {
    r: tokio::io::ReadHalf<local::Stream>,
    w: tokio::io::WriteHalf<local::Stream>,
}

impl Ctl {
    async fn connect(rig: &Rig, token: &str) -> anyhow::Result<(Ctl, Msg)> {
        let mut s = local::connect(&rig.channel, std::process::id()).await?;
        proto::write(&mut s, &Frame::Msg(Msg::Hello { token: token.into(), proto: PROTO })).await?;
        let (mut r, w) = tokio::io::split(s);
        match tokio::time::timeout(Duration::from_secs(3), proto::read(&mut r, MAX_FRAME)).await?? {
            Some(Frame::Msg(m @ Msg::Welcome { .. })) => Ok((Ctl { r, w }, m)),
            other => anyhow::bail!("no welcome: {other:?}"),
        }
    }

    async fn send(&mut self, m: Msg) {
        proto::write(&mut self.w, &Frame::Msg(m)).await.unwrap();
    }

    async fn next(&mut self) -> Frame {
        tokio::time::timeout(Duration::from_secs(5), proto::read(&mut self.r, MAX_FRAME)).await.expect("a frame in time").unwrap().expect("channel open")
    }

    /// The next message, skipping ping readings.
    async fn msg(&mut self) -> Msg {
        loop {
            if let Frame::Msg(m) = self.next().await
                && !matches!(m, Msg::Pings { .. })
            {
                return m;
            }
        }
    }

    async fn listen(&mut self, port: u16) {
        self.send(Msg::Listen { bind: "127.0.0.1".into(), port: Some(port) }).await;
        assert_eq!(self.msg().await, Msg::Listening { port: Some(port), error: None });
    }

    /// What hopperd does for a join to a running server: reads the
    /// handshake and login from the leg, then splices with those as replay.
    async fn route_to(&mut self, server: u16, read_upto: usize) -> u64 {
        let conn = loop {
            if let Msg::Open { conn, .. } = self.msg().await {
                break conn;
            }
        };
        let mut got = Vec::new();
        while got.len() < read_upto {
            match self.next().await {
                Frame::Data(c, b) if c == conn => got.extend_from_slice(&b),
                _ => {}
            }
        }
        // hopperd read exactly `read_upto` (the rest stays in the edge's record).
        got.truncate(read_upto);
        self.send(Msg::Splice { conn, port: server, replay: proto::b64(&got), consumed: read_upto as u64, server: "srv".into(), player: "Steve".into() }).await;
        loop {
            match self.next().await {
                Frame::Msg(Msg::Spliced { conn: c, ok, error, pipe }) if c == conn => {
                    assert!(ok, "splice failed: {error:?}");
                    // Named, so a hopperd that stopped waiting still lists the player.
                    let p = pipe.expect("a good splice names its pipe");
                    assert_eq!((p.conn, p.server.as_str(), p.player.as_str()), (conn, "srv", "Steve"));
                    return conn;
                }
                _ => {}
            }
        }
    }
}

/// Waits until the edge has noticed hopperd go. A join before that would be
/// sent down the dead channel instead of waiting for the next hopperd, and
/// how long the edge takes depends on how busy the machine is.
async fn hopperd_gone(rig: &Rig) {
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while rig.edge.hopperd_connected() {
        assert!(std::time::Instant::now() < end, "the edge never noticed hopperd go");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn login(name: &str, port: u16) -> Vec<u8> {
    let mut b = core::handshake_packet(767, "srv.mc.test", port, 2);
    let mut body = Vec::new();
    core::write_string(&mut body, name);
    body.extend_from_slice(&[0u8; 16]);
    b.extend_from_slice(&core::packet(0, &body));
    b
}

/// A fake Minecraft server: checks the first bytes are exactly `expect`,
/// then sends a numbered line every 20 ms and echoes every line it gets.
async fn fake_server(expect_each: Vec<u8>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let expect = expect_each.clone();
            tokio::spawn(async move {
                let (r, mut w) = s.into_split();
                let mut r = BufReader::new(r);
                let mut head = vec![0u8; expect.len()];
                if r.read_exact(&mut head).await.is_err() || head != expect {
                    let _ = w.write_all(b"BAD HEAD\n").await;
                    return;
                }
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                tokio::spawn(async move {
                    let mut line = String::new();
                    while r.read_line(&mut line).await.is_ok_and(|n| n > 0) {
                        let _ = tx.send(format!("echo {}", line.trim()));
                        line.clear();
                    }
                });
                let mut n = 0u64;
                let mut tick = tokio::time::interval(Duration::from_millis(20));
                loop {
                    tokio::select! {
                        _ = tick.tick() => {
                            n += 1;
                            if w.write_all(format!("{n}\n").as_bytes()).await.is_err() { return; }
                        }
                        m = rx.recv() => match m {
                            Some(m) => if w.write_all(format!("{m}\n").as_bytes()).await.is_err() { return; },
                            None => return,
                        },
                    }
                }
            });
        }
    });
    port
}

/// Reads numbered lines, checking each is the one after the last; returns
/// the last number seen and any echoes.
async fn read_numbers(r: &mut BufReader<tokio::net::tcp::OwnedReadHalf>, count: usize, last: &mut u64, echoes: &mut Vec<String>) {
    let mut seen = 0;
    while seen < count {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(5), r.read_line(&mut line)).await.expect("no gap over 5 s").unwrap();
        assert!(n > 0, "the player was disconnected after number {last}");
        let line = line.trim();
        if let Some(e) = line.strip_prefix("echo ") {
            echoes.push(e.to_string());
            continue;
        }
        let v: u64 = line.parse().unwrap_or_else(|_| panic!("unexpected line {line:?}"));
        assert_eq!(v, *last + 1, "numbers must come with no gap and no repeat");
        *last = v;
        seen += 1;
    }
}

#[tokio::test]
async fn frames_round_trip() {
    let mut buf = Vec::new();
    let frames = [Frame::Msg(Msg::Close { conn: 7 }), Frame::Data(9, b"hello".to_vec()), Frame::Msg(Msg::Pings { list: vec![(1, Some(12)), (2, None)] })];
    for f in &frames {
        proto::write(&mut buf, f).await.unwrap();
    }
    let mut r = &buf[..];
    for f in frames {
        assert_eq!(proto::read(&mut r, MAX_FRAME).await.unwrap(), Some(f));
    }
    assert_eq!(proto::read(&mut r, MAX_FRAME).await.unwrap(), None);
    // A length past the limit is refused before anything is read into memory.
    let mut big = ((MAX_FRAME + 10) as u32).to_le_bytes().to_vec();
    big.push(0);
    assert!(proto::read(&mut &big[..], MAX_FRAME).await.is_err());
}

/// The main promise: a player spliced to a server keeps every byte while
/// hopperd goes away and comes back, and a join in the gap is held, then
/// handed to the new hopperd.
#[tokio::test]
async fn players_stay_on_while_hopperd_restarts() {
    let rig = rig(Duration::from_secs(20)).await;
    let hello = login("Steve", rig.port);
    // What the server must receive first: the login, then the player's extra
    // bytes, exactly once (hopperd reads only the login; the edge replays the
    // rest from its own record).
    let mut head = hello.clone();
    head.extend_from_slice(b"extra\n");
    let server = fake_server(head.clone()).await;

    let (mut ctl, _) = Ctl::connect(&rig, &rig.token).await.unwrap();
    ctl.listen(rig.port).await;

    let p1 = TcpStream::connect(("127.0.0.1", rig.port)).await.unwrap();
    let (r1, mut w1) = p1.into_split();
    // In odd pieces, the extra line right behind the login.
    for piece in head.chunks(7) {
        w1.write_all(piece).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    ctl.route_to(server, hello.len()).await;
    let mut r1 = BufReader::new(r1);
    let (mut last, mut echoes) = (0u64, Vec::new());
    read_numbers(&mut r1, 10, &mut last, &mut echoes).await;
    // The server checked its first bytes were exactly the login and the
    // extra line; a second copy would come back as an echo.
    assert!(!echoes.contains(&"extra".to_string()), "the bytes past the login reach the server once: {echoes:?}");

    // hopperd is killed.
    drop(ctl);
    hopperd_gone(&rig).await;
    w1.write_all(b"still here\n").await.unwrap();
    read_numbers(&mut r1, 25, &mut last, &mut echoes).await;
    assert!(echoes.contains(&"still here".to_string()));

    // A join while hopperd is away waits on the connecting screen...
    let p2 = TcpStream::connect(("127.0.0.1", rig.port)).await.unwrap();
    let (r2, mut w2) = p2.into_split();
    let hello2 = login("Alex", rig.port);
    w2.write_all(&hello2).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ...and the new hopperd gets it, with what it sent so far.
    let (mut ctl, welcome) = Ctl::connect(&rig, &rig.token).await.unwrap();
    let Msg::Welcome { pipes, listening, .. } = welcome else { unreachable!() };
    assert_eq!(listening, Some(rig.port), "the port stayed open");
    assert_eq!(pipes.len(), 1, "the new hopperd hears who is on");
    assert_eq!(pipes[0].player, "Steve");
    let server2 = fake_server(hello2.clone()).await;
    ctl.route_to(server2, hello2.len()).await;
    let mut r2 = BufReader::new(r2);
    let (mut last2, mut e2) = (0u64, Vec::new());
    read_numbers(&mut r2, 5, &mut last2, &mut e2).await;

    // And the first player never noticed any of it.
    read_numbers(&mut r1, 10, &mut last, &mut echoes).await;
    drop(w2);
}

#[tokio::test]
async fn a_held_join_is_told_to_come_back_when_hopperd_stays_away() {
    let rig = rig(Duration::from_millis(600)).await;
    let (mut ctl, _) = Ctl::connect(&rig, &rig.token).await.unwrap();
    ctl.listen(rig.port).await;
    drop(ctl);
    hopperd_gone(&rig).await;
    let mut p = TcpStream::connect(("127.0.0.1", rig.port)).await.unwrap();
    p.write_all(&login("Alex", rig.port)).await.unwrap();
    let mut got = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), p.read_to_end(&mut got)).await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&got).contains("Join again in a few seconds"));
}

#[tokio::test]
async fn the_server_list_is_answered_while_hopperd_is_away() {
    let rig = rig(Duration::from_secs(20)).await;
    let (mut ctl, _) = Ctl::connect(&rig, &rig.token).await.unwrap();
    ctl.listen(rig.port).await;
    let kept = json!({ "version": { "name": "1.21.1", "protocol": 767 }, "players": { "max": 10, "online": 2 } });
    ctl.send(Msg::Answers { list: vec![Answer { addresses: vec!["srv.mc.test".into()], status: kept }], unknown: json!({}) }).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(ctl);
    hopperd_gone(&rig).await;
    let mut p = TcpStream::connect(("127.0.0.1", rig.port)).await.unwrap();
    let mut out = core::handshake_packet(767, "srv.mc.test", rig.port, 1);
    out.extend_from_slice(&[1, 0]);
    out.extend_from_slice(&core::packet(1, &42u64.to_be_bytes()));
    p.write_all(&out).await.unwrap();
    let mut got = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), p.read_to_end(&mut got)).await.unwrap().unwrap();
    let text = String::from_utf8_lossy(&got);
    assert!(text.contains(RESTART_MOTD), "{text}");
    assert!(text.contains("1.21.1"), "the last real answer is kept: {text}");
}

/// The channel takes orders only with the token, and the edge never
/// connects anywhere but this machine, nor back into itself.
#[tokio::test]
async fn wrong_token_and_bad_splices_are_refused() {
    let rig = rig(Duration::from_secs(20)).await;
    assert!(Ctl::connect(&rig, "not-the-token").await.is_err());
    assert!(Ctl::connect(&rig, &"t".repeat(63)).await.is_err());
    // The edge keeps running for the real one.
    let (mut ctl, _) = Ctl::connect(&rig, &rig.token).await.unwrap();
    ctl.listen(rig.port).await;
    let mut p = TcpStream::connect(("127.0.0.1", rig.port)).await.unwrap();
    p.write_all(&login("Steve", rig.port)).await.unwrap();
    let conn = loop {
        if let Msg::Open { conn, .. } = ctl.msg().await {
            break conn;
        }
    };
    for port in [rig.port, 0] {
        ctl.send(Msg::Splice { conn, port, replay: String::new(), consumed: 0, server: String::new(), player: String::new() }).await;
        loop {
            if let Msg::Spliced { ok, .. } = ctl.msg().await {
                assert!(!ok, "port {port} must be refused");
                break;
            }
        }
    }
    // More than the player sent can't be "consumed".
    ctl.send(Msg::Splice { conn, port: 1, replay: String::new(), consumed: 1 << 20, server: String::new(), player: String::new() }).await;
    loop {
        if let Msg::Spliced { ok, .. } = ctl.msg().await {
            assert!(!ok);
            break;
        }
    }
    drop(p);
    let _ = rig.edge;
}

#[tokio::test]
async fn too_many_wrong_tokens_close_the_channel_for_a_while() {
    let rig = rig(Duration::from_secs(20)).await;
    for _ in 0..10 {
        assert!(Ctl::connect(&rig, "wrong").await.is_err());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Even the right token waits now (the channel isn't read for a minute).
    assert!(tokio::time::timeout(Duration::from_secs(2), Ctl::connect(&rig, &rig.token)).await.map_or(true, |r| r.is_err()));
}

#[test]
fn a_run_file_is_only_taken_for_a_live_edge() {
    let me = std::process::id();
    let (created, exe) = super::proc_info(me).expect("this process");
    let mut r = super::RunFile { pid: me, created, version: String::new(), proto: PROTO, channel: String::new(), token: String::new() };
    // This test's own program isn't a router.
    assert!(!super::names_edge(&exe));
    assert!(!super::is_ours(&r));
    r.created = created + 1;
    assert!(!super::is_ours(&r));
    r.pid = u32::MAX - 1;
    assert!(!super::is_ours(&r));
    assert!(super::names_edge(std::path::Path::new(&format!("/x/{}", super::PROGRAM))));
    assert!(super::names_edge(std::path::Path::new(&format!("/x/{} (deleted)", super::PROGRAM))));
}

/// Only this account (and SYSTEM) may open the pipe: a token of this same
/// user with its own SID turned off (as another account would look to the
/// pipe) is refused by Windows itself.
#[cfg(windows)]
#[tokio::test]
async fn another_account_cant_open_the_pipe() {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, GetTokenInformation, ImpersonateLoggedOnUser, RevertToSelf, SID_AND_ATTRIBUTES, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
        TOKEN_IMPERSONATE, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let sddl = local::sddl().unwrap();
    assert!(sddl.starts_with("D:P(A;;GA;;;SY)(A;;GA;;;S-1-"), "{sddl}");
    assert_eq!(sddl.matches("(A;").count(), 2, "nobody but SYSTEM and this account: {sddl}");

    let rig = rig(Duration::from_secs(20)).await;
    let name = rig.channel.clone();
    let opened_as_other = std::thread::spawn(move || unsafe {
        let mut tok = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE | TOKEN_ASSIGN_PRIMARY, &mut tok).unwrap();
        let mut len = 0u32;
        let _ = GetTokenInformation(tok, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        GetTokenInformation(tok, TokenUser, Some(buf.as_mut_ptr() as *mut _), len, &mut len).unwrap();
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let disable = [SID_AND_ATTRIBUTES { Sid: user.User.Sid, Attributes: 0 }];
        let mut other = HANDLE::default();
        CreateRestrictedToken(tok, DISABLE_MAX_PRIVILEGE, Some(&disable), None, None, &mut other).unwrap();
        ImpersonateLoggedOnUser(other).unwrap();
        let r = std::fs::OpenOptions::new().read(true).write(true).open(&name);
        let _ = RevertToSelf();
        let _ = CloseHandle(other);
        let _ = CloseHandle(tok);
        r.map(|_| ()).map_err(|e| e.kind())
    })
    .join()
    .unwrap();
    assert_eq!(opened_as_other, Err(std::io::ErrorKind::PermissionDenied));
    // This account still gets in.
    assert!(Ctl::connect(&rig, &rig.token).await.is_ok());
}

/// The pipe's name is held by someone else first: the edge refuses to share
/// it, and hopperd refuses to talk to a pipe the router doesn't hold.
#[cfg(windows)]
#[tokio::test]
async fn a_squatted_pipe_is_refused_both_ways() {
    let name = channel_name();
    let _squatter = tokio::net::windows::named_pipe::ServerOptions::new().first_pipe_instance(true).create(&name).unwrap();
    assert!(local::Listener::bind(&name).is_err());
    // Held by this process, not by the pid the run file names.
    assert!(local::connect(&name, std::process::id() + 1).await.is_err());
}

/// An edge from another Hopper Server version answers the hello with its own
/// channel version and its players, and takes no orders: hopperd learns
/// both (and replaces it once nobody is on; see `client::supervise`).
#[tokio::test]
async fn an_edge_of_another_version_is_told_apart() {
    use super::client::{Refused, hello};
    let rig = rig(Duration::from_secs(20)).await;
    // This hopperd pretends to be a newer one.
    let mut s = local::connect(&rig.channel, std::process::id()).await.unwrap();
    match hello(&mut s, &rig.token, PROTO + 1).await {
        Err(Refused::Version { theirs, pipes }) => assert_eq!((theirs, pipes), (PROTO, 0)),
        other => panic!("expected a version mismatch, got {other:?}"),
    }
    // It isn't taken as this edge's hopperd: a matching one still connects.
    let mut s = local::connect(&rig.channel, std::process::id()).await.unwrap();
    assert!(hello(&mut s, &rig.token, PROTO).await.is_ok());
}

#[test]
fn the_channel_is_retried_ever_more_slowly() {
    use super::client::backoff;
    assert_eq!(backoff(0), Duration::from_millis(200));
    assert!((0..20).all(|n| backoff(n) <= backoff(n + 1)));
    assert_eq!(backoff(50), Duration::from_secs(30));
}

/// A port the edge couldn't have is asked for again every router round
/// until it listens there; a port it has isn't asked for again.
#[test]
fn a_port_not_had_yet_is_asked_for_again() {
    use super::client::needs_listen;
    let want = Some(("0.0.0.0".to_string(), 25565));
    assert!(needs_listen(&None, &want, None));
    assert!(needs_listen(&want, &want, None), "couldn't bind last time: ask again");
    assert!(needs_listen(&want, &want, Some(25566)));
    assert!(!needs_listen(&want, &want, Some(25565)));
    assert!(needs_listen(&want, &None, Some(25565)), "router off: stop");
    assert!(!needs_listen(&None, &None, None));
}

#[test]
fn a_start_failure_is_said_plainly() {
    let e = anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied)).context(r"starting C:\x\hopperd-edge.exe");
    let s = super::client::start_error(&e);
    assert!(s.starts_with("The router couldn't start (Windows or an antivirus didn't let its program run)"), "{s}");
    assert!(!s.contains(r"C:\x"), "no paths or raw errors: {s}");
    assert!(super::client::start_error(&anyhow::anyhow!("no place for the router's program")).contains("no folder its program could be copied to"));
}

/// Two edges started for one folder at once: only the first gets the lock,
/// and the second one leaves (it can't take the first's channel).
#[test]
fn only_one_edge_holds_a_folder() {
    let dir = std::env::temp_dir().join(format!("hopperd-edge-lock-{}", std::process::id()));
    super::private_folder(&dir).unwrap();
    let first = super::take_lock(&dir).unwrap();
    assert!(first.is_some());
    assert!(super::take_lock(&dir).unwrap().is_none());
    drop(first);
    assert!(super::take_lock(&dir).unwrap().is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_long_data_folder_gets_a_short_socket() {
    let short = std::path::Path::new("/var/lib/hopperd");
    assert_eq!(super::socket_path(short, None, 1000), short.join("edge").join("edge.sock"));
    let long = std::path::PathBuf::from(format!("/home/someone/{}", "deep/".repeat(30)));
    let p = super::socket_path(&long, Some(std::path::Path::new("/run/user/1000")), 1000);
    assert!(p.starts_with("/run/user/1000") && p.as_os_str().len() <= 107, "{}", p.display());
    // A runtime folder that's long too, or none: /tmp, named for this account.
    let p = super::socket_path(&long, Some(&long), 1000);
    assert!(p.to_string_lossy().starts_with("/tmp/hopperd-edge-1000-") && p.as_os_str().len() <= 107, "{}", p.display());
    assert_eq!(super::socket_path(&long, None, 1000), p);
    // Two data folders don't share one.
    assert_ne!(super::socket_path(&long.join("a"), None, 1000), p);
}

/// The service (SYSTEM) never runs the router from the data folder.
#[test]
fn a_service_runs_the_router_only_from_beside_its_exe() {
    let exe = std::path::Path::new("/opt/hopper");
    let data = std::path::Path::new("/data");
    assert_eq!(super::paths_for(Some(exe), data, true), vec![exe.join(super::PROGRAM)]);
    assert_eq!(super::paths_for(Some(exe), data, false), vec![exe.join(super::PROGRAM), super::edge_dir(data).join(super::PROGRAM)]);
    assert_eq!(super::paths_for(None, data, false), vec![super::edge_dir(data).join(super::PROGRAM)]);
}

#[test]
fn the_firewall_line_says_what_to_do() {
    assert_eq!(super::firewall_note(Some("Allow\r\n")), None);
    assert_eq!(super::firewall_note(None), None, "couldn't tell: no line");
    let blocked = super::firewall_note(Some("Allow\r\nBlock\r\n")).unwrap();
    assert!(blocked.starts_with("Windows Firewall blocks the router"), "{blocked}");
    let none = super::firewall_note(Some("")).unwrap();
    assert!(none.contains("If Windows asks about it, choose Allow"), "{none}");
}

/// Turning the in-panel router on (or the router off) tells the edge to stop
/// listening: the port is free at once while players on it stay.
#[tokio::test]
async fn a_draining_edge_lets_go_of_the_port() {
    let rig = rig(Duration::from_secs(20)).await;
    let server = fake_server(Vec::new()).await;
    let (mut ctl, _) = Ctl::connect(&rig, &rig.token).await.unwrap();
    ctl.listen(rig.port).await;
    let mut player = TcpStream::connect(("127.0.0.1", rig.port)).await.unwrap();
    let hello = login("Steve", rig.port);
    player.write_all(&hello).await.unwrap();
    ctl.route_to(server, hello.len()).await;
    ctl.send(Msg::Stop { drain: true }).await;
    // Wait for the edge to let go, with room for a machine busy with other
    // tests (the drain message and the rebind both wait on the scheduler;
    // another test's outgoing socket can also hold the port for a moment).
    let t = std::time::Instant::now();
    let freed = loop {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", rig.port)).await {
            break l;
        }
        assert!(t.elapsed() < Duration::from_secs(15), "the port stayed taken");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    drop(freed);
    assert_eq!(rig.edge.pipes().len(), 1, "the player on a server stays");
}

#[test]
fn ending_an_edge_that_isnt_there_is_done() {
    let dir = std::env::temp_dir().join(format!("hopperd-edge-none-{}", std::process::id()));
    assert!(super::end_for(&dir, Duration::from_millis(10)));
}

/// Each edge's pipe gets a name nobody can know before it starts.
#[cfg(windows)]
#[test]
fn the_pipe_name_has_a_random_part() {
    let d = std::path::Path::new(r"C:\data");
    let a = local::address(d, "0123abcd").unwrap();
    let b = local::address(d, "fedc9876").unwrap();
    assert!(a.starts_with(r"\\.\pipe\hopperd-edge-") && a.ends_with("-0123abcd"), "{a}");
    assert_ne!(a, b);
}

/// A symlink where the edge folder goes is refused before anything is
/// changed: its target keeps its permissions.
#[cfg(unix)]
#[test]
fn a_symlinked_edge_folder_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let base = std::env::temp_dir().join(format!("hopperd-edge-link-{}", std::process::id()));
    let target = base.join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let link = base.join("edge");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(super::private_folder(&link).is_err());
    assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o755);
    let _ = std::fs::remove_dir_all(&base);
}

/// A socket another edge still answers on isn't deleted by a second one.
#[cfg(unix)]
#[tokio::test]
async fn a_live_socket_isnt_taken_over() {
    let name = channel_name();
    let _first = local::Listener::bind(&name).unwrap();
    assert!(local::Listener::bind(&name).is_err());
    assert!(std::path::Path::new(&name).exists());
}
