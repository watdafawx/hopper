//! Holding a modded player in the login phase while their server starts.
//!
//! A Forge / NeoForge client with mods refuses a server that isn't running
//! the same mods, and the refusal happens in the *configuration* phase,
//! after the router has sent Login Success (so the lobby island can't take
//! them). Before that, in the login phase, the client only waits for the
//! server. This keeps it waiting there: a Login Plugin Request every few
//! seconds (any packet resets the client's read timeout; a client that
//! doesn't know the channel answers "not understood"), and when the real
//! server is up the caller replays the player's Handshake and Login Start to
//! it and passes the connection on. The player stays on the "Connecting to
//! the server..." screen and joins by itself. There is no progress bar.
//!
//! This is the state machine only, over any async stream, so it can be
//! tested without sockets. The panel wires it up in `router.rs`.

use anyhow::{Context as _, bail};
use hopper_core::hosting::limbo as lb;
use hopper_core::hosting::router::{self as core, Read};
use std::future::Future;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::time::Instant;

/// The channel of the keep-alive request. Clients don't know it.
pub const CHANNEL: &str = "hopper:wait";

/// What the server is doing right now, from the panel.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Progress {
    Waiting,
    /// Up and answering on its port.
    Ready,
    /// Stopped or crashed before it was up.
    Failed,
}

/// How a hold ended. The caller does what follows (hand over, kick).
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// The server is up and the client is between packets: hand the
    /// connection over. The bytes are anything the client sent that the hold
    /// didn't use (to be forwarded after the replayed login).
    Ready(Vec<u8>),
    /// The start failed: tell the player.
    Failed,
    /// The wait ran out: tell the player to join again.
    Capped,
    /// The client left.
    Left,
    /// The client stopped answering (it is gone or not a real client).
    NoAnswer,
}

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// How often the server's state is looked at.
    pub poll: Duration,
    /// How often the client gets a request. Its read timeout is 30 s.
    pub ping_every: Duration,
    /// How long a request may stay unanswered.
    pub answer_within: Duration,
    /// The longest a player is held.
    pub cap: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { poll: Duration::from_secs(1), ping_every: Duration::from_secs(10), answer_within: Duration::from_secs(20), cap: Duration::from_secs(10 * 60) }
    }
}

/// A client sends nothing but answers in the login phase; this is far more
/// than any of those.
const MAX_BUFFERED: usize = 8 * 1024;
/// A write that doesn't finish in this time means the client isn't reading.
const WRITE_WITHIN: Duration = Duration::from_secs(10);
/// First message id; they count up from here.
const FIRST_ID: i32 = 0x4850;

async fn send<S: AsyncWrite + Unpin>(sock: &mut S, bytes: &[u8]) -> anyhow::Result<()> {
    tokio::time::timeout(WRITE_WITHIN, async {
        sock.write_all(bytes).await?;
        sock.flush().await
    })
    .await
    .context("client not reading")??;
    Ok(())
}

/// Holds `sock` (a client that just sent its Login Start; `buf` is what it
/// sent after that) until the server is up, fails, the cap passes or the
/// client goes. `progress` looks at the server.
pub async fn hold<S, F, Fut>(sock: &mut S, mut buf: Vec<u8>, t: Timing, mut progress: F) -> anyhow::Result<Ended>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnMut() -> Fut,
    Fut: Future<Output = Progress>,
{
    let began = Instant::now();
    let mut next_id = FIRST_ID;
    // The request waiting for its answer, and when the last one went out.
    let mut pending: Option<(i32, Instant)> = None;
    let mut last_sent: Option<Instant> = None;
    // The server is up; waiting for the client's last answer so nothing of
    // ours is left unanswered when the real server takes over.
    let mut ready = false;
    let mut tick = tokio::time::interval(t.poll);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut chunk = [0u8; 1024];
    loop {
        tokio::select! {
            r = sock.read(&mut chunk) => {
                let n = r?;
                if n == 0 {
                    return Ok(Ended::Left);
                }
                buf.extend_from_slice(&chunk[..n]);
                while let Read::Done((id, body), used) = core::read_packet(&buf, 4096)? {
                    // In the login phase the client only answers requests.
                    if id != lb::LOGIN_PLUGIN_RESPONSE {
                        bail!("unexpected packet {id:#x} while held");
                    }
                    let answered = lb::login_plugin_response_id(body)?;
                    match pending {
                        Some((want, _)) if want == answered => pending = None,
                        _ => bail!("answer to a request that wasn't sent"),
                    }
                    buf.drain(..used);
                }
                if buf.len() > MAX_BUFFERED {
                    bail!("too much from the client while held");
                }
                if ready && pending.is_none() {
                    return Ok(Ended::Ready(buf));
                }
            }
            _ = tick.tick() => {
                if pending.is_some_and(|(_, at)| at.elapsed() >= t.answer_within) {
                    return Ok(Ended::NoAnswer);
                }
                if !ready {
                    if began.elapsed() >= t.cap {
                        return Ok(Ended::Capped);
                    }
                    match progress().await {
                        Progress::Failed => return Ok(Ended::Failed),
                        Progress::Ready => {
                            ready = true;
                            if pending.is_none() {
                                return Ok(Ended::Ready(buf));
                            }
                        }
                        Progress::Waiting => {}
                    }
                }
                if !ready && pending.is_none() && last_sent.is_none_or(|l| l.elapsed() >= t.ping_every) {
                    let id = next_id;
                    next_id += 1;
                    send(sock, &lb::login_plugin_request(id, CHANNEL, &[])).await?;
                    pending = Some((id, Instant::now()));
                    last_sent = pending.map(|p| p.1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU8, Ordering};
    use tokio::io::duplex;

    const FAST: Timing = Timing { poll: Duration::from_millis(5), ping_every: Duration::from_millis(40), answer_within: Duration::from_millis(300), cap: Duration::from_secs(5) };

    /// A progress source the test flips: 0 waiting, 1 ready, 2 failed.
    fn state(v: u8) -> Arc<AtomicU8> {
        Arc::new(AtomicU8::new(v))
    }
    fn look(s: &Arc<AtomicU8>) -> impl FnMut() -> std::future::Ready<Progress> + use<> {
        let s = s.clone();
        move || {
            std::future::ready(match s.load(Ordering::SeqCst) {
                0 => Progress::Waiting,
                1 => Progress::Ready,
                _ => Progress::Failed,
            })
        }
    }

    /// Reads one packet from the "client" side of the duplex.
    async fn client_read(c: &mut tokio::io::DuplexStream, buf: &mut Vec<u8>) -> (i32, Vec<u8>) {
        loop {
            if let Read::Done((id, body), used) = core::read_packet(buf, 4096).unwrap() {
                let out = (id, body.to_vec());
                buf.drain(..used);
                return out;
            }
            let mut chunk = [0u8; 256];
            let n = tokio::time::timeout(Duration::from_secs(3), c.read(&mut chunk)).await.expect("client read timed out").unwrap();
            assert!(n > 0, "closed");
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    fn answer(id: i32) -> Vec<u8> {
        let mut body = Vec::new();
        core::write_varint(&mut body, id);
        body.push(0); // not understood
        core::packet(lb::LOGIN_PLUGIN_RESPONSE, &body)
    }

    #[test]
    fn request_layout() {
        // varint id, identifier string, then data.
        let p = lb::login_plugin_request(0x4850, CHANNEL, &[1, 2]);
        let Read::Done((id, body), used) = core::read_packet(&p, 4096).unwrap() else { panic!("cut short") };
        assert_eq!((id, used), (0x04, p.len()));
        let (mid, n) = core::read_varint(body).unwrap().unwrap();
        assert_eq!(mid, 0x4850);
        let (chan, m) = core::read_string(&body[n..], 256).unwrap();
        assert_eq!(chan, "hopper:wait");
        assert_eq!(&body[n + m..], &[1, 2]);
        assert_eq!(lb::login_plugin_response_id(&answer(0x4850)[2..]).unwrap(), 0x4850);
    }

    #[tokio::test]
    async fn keeps_asking_and_hands_over_when_ready() {
        let (mut server_side, mut client) = duplex(4096);
        let s = state(0);
        let task = tokio::spawn({
            let s = s.clone();
            async move { hold(&mut server_side, vec![], FAST, look(&s)).await.unwrap() }
        });
        let mut cb = Vec::new();
        let mut ids = Vec::new();
        for _ in 0..3 {
            let (id, body) = client_read(&mut client, &mut cb).await;
            assert_eq!(id, lb::LOGIN_PLUGIN_REQUEST);
            let (mid, _) = core::read_varint(&body).unwrap().unwrap();
            ids.push(mid);
            client.write_all(&answer(mid)).await.unwrap();
        }
        assert_eq!(ids, vec![FIRST_ID, FIRST_ID + 1, FIRST_ID + 2], "one at a time, counting up");
        s.store(1, Ordering::SeqCst);
        assert_eq!(task.await.unwrap(), Ended::Ready(vec![]));
    }

    #[tokio::test]
    async fn ready_waits_for_the_last_answer() {
        let (mut server_side, mut client) = duplex(4096);
        let s = state(0);
        let task = tokio::spawn({
            let s = s.clone();
            async move { hold(&mut server_side, vec![], FAST, look(&s)).await.unwrap() }
        });
        let mut cb = Vec::new();
        let (_, body) = client_read(&mut client, &mut cb).await;
        let (mid, _) = core::read_varint(&body).unwrap().unwrap();
        // The server comes up while the request is unanswered: no handover yet.
        s.store(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(!task.is_finished(), "an unanswered request would reach the real server");
        client.write_all(&answer(mid)).await.unwrap();
        assert_eq!(task.await.unwrap(), Ended::Ready(vec![]));
    }

    #[tokio::test]
    async fn client_leaves() {
        let (mut server_side, client) = duplex(4096);
        let s = state(0);
        let task = tokio::spawn(async move { hold(&mut server_side, vec![], FAST, look(&s)).await.unwrap() });
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(client);
        assert_eq!(task.await.unwrap(), Ended::Left);
    }

    #[tokio::test]
    async fn start_fails() {
        let (mut server_side, _client) = duplex(4096);
        let s = state(0);
        let task = tokio::spawn({
            let s = s.clone();
            async move { hold(&mut server_side, vec![], FAST, look(&s)).await.unwrap() }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        s.store(2, Ordering::SeqCst);
        assert_eq!(task.await.unwrap(), Ended::Failed);
    }

    #[tokio::test]
    async fn cap_reached() {
        let (mut server_side, mut client) = duplex(4096);
        let s = state(0);
        let t = Timing { cap: Duration::from_millis(120), ..FAST };
        let task = tokio::spawn(async move { hold(&mut server_side, vec![], t, look(&s)).await.unwrap() });
        // Answers keep it alive, but not past the cap.
        tokio::spawn(async move {
            let mut cb = Vec::new();
            let mut chunk = [0u8; 256];
            loop {
                while let Ok(Read::Done((_, body), used)) = core::read_packet(&cb, 4096) {
                    let (mid, _) = core::read_varint(body).unwrap().unwrap();
                    cb.drain(..used);
                    if client.write_all(&answer(mid)).await.is_err() {
                        return;
                    }
                }
                match client.read(&mut chunk).await {
                    Ok(n) if n > 0 => cb.extend_from_slice(&chunk[..n]),
                    _ => return,
                }
            }
        });
        assert_eq!(task.await.unwrap(), Ended::Capped);
    }

    #[tokio::test]
    async fn client_never_answers() {
        let (mut server_side, _client) = duplex(4096);
        let s = state(0);
        let t = Timing { answer_within: Duration::from_millis(60), ..FAST };
        let started = Instant::now();
        let ended = hold(&mut server_side, vec![], t, look(&s)).await.unwrap();
        assert_eq!(ended, Ended::NoAnswer);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn junk_from_the_client_is_dropped() {
        // A packet that isn't an answer, an answer to nothing, and a flood.
        for bytes in [core::packet(0x03, &[]), answer(7), vec![0xff; 5], vec![0u8; MAX_BUFFERED + 1]] {
            let (mut server_side, mut client) = duplex(1 << 16);
            let s = state(0);
            client.write_all(&bytes).await.unwrap();
            let r = hold(&mut server_side, vec![], FAST, look(&s)).await;
            assert!(r.is_err() || r.as_ref().is_ok_and(|e| *e != Ended::Ready(vec![])), "{r:?}");
        }
    }

    #[tokio::test]
    async fn bytes_after_the_answer_are_kept_for_the_server() {
        let (mut server_side, mut client) = duplex(4096);
        let s = state(0);
        let task = tokio::spawn({
            let s = s.clone();
            async move { hold(&mut server_side, vec![], FAST, look(&s)).await.unwrap() }
        });
        let mut cb = Vec::new();
        let (_, body) = client_read(&mut client, &mut cb).await;
        let (mid, _) = core::read_varint(&body).unwrap().unwrap();
        s.store(1, Ordering::SeqCst);
        // The answer, then the start of a next packet (length 5, no body yet).
        let mut out = answer(mid);
        out.push(5);
        client.write_all(&out).await.unwrap();
        assert_eq!(task.await.unwrap(), Ended::Ready(vec![5]));
    }
}
