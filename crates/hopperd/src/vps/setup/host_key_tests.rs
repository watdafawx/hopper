//! The host key is read and confirmed before any secret leaves the panel
//! (review finding M2). A small ssh server on 127.0.0.1 stands in for the VPS
//! and counts every sign-in it is offered; nothing here waits on a timer.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use axum::http::StatusCode;
use russh::keys::PrivateKey;
use russh::keys::ssh_key::private::{Ed25519Keypair, KeypairData};
use russh::server::{Auth, ChannelOpenHandle, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use serde_json::{Value, json};

use super::ssh::{self, Auth as Cred, Ssh};
use crate::cross_origin_tests::{OWN, T, body, call, cookie_login, panel_with};

#[derive(Default)]
struct Counts {
    password: AtomicUsize,
    key: AtomicUsize,
    /// Commands run after a sign-in.
    exec: AtomicUsize,
}

struct Fake {
    port: u16,
    fp: String,
    counts: Arc<Counts>,
}

struct FakeServer(Arc<Counts>);

struct FakeSession(Arc<Counts>);

impl russh::server::Server for FakeServer {
    type Handler = FakeSession;
    fn new_client(&mut self, _: Option<SocketAddr>) -> FakeSession {
        FakeSession(self.0.clone())
    }
}

impl russh::server::Handler for FakeSession {
    type Error = russh::Error;

    async fn auth_password(&mut self, _: &str, _: &str) -> Result<Auth, Self::Error> {
        self.0.password.fetch_add(1, SeqCst);
        Ok(Auth::Accept)
    }

    async fn auth_publickey(&mut self, _: &str, _: &russh::keys::ssh_key::PublicKey) -> Result<Auth, Self::Error> {
        self.0.key.fetch_add(1, SeqCst);
        Ok(Auth::Accept)
    }

    async fn channel_open_session(&mut self, _: Channel<Msg>, reply: ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    // Every command answers "0" (so `id -u` says root) and ends well.
    async fn exec_request(&mut self, id: ChannelId, _: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        self.0.exec.fetch_add(1, SeqCst);
        session.channel_success(id)?;
        session.data(id, b"0\n".to_vec())?;
        session.exit_status_request(id, 0)?;
        session.eof(id)?;
        session.close(id)?;
        Ok(())
    }
}

fn host_key() -> (PrivateKey, String) {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).unwrap();
    let key = PrivateKey::new(KeypairData::Ed25519(Ed25519Keypair::from_seed(&seed)), "fake-vps").unwrap();
    let fp = key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string();
    (key, fp)
}

async fn fake_vps() -> Fake {
    fake_vps_on(0).await
}

async fn fake_vps_on(port: u16) -> Fake {
    let (key, fp) = host_key();
    let counts = Arc::new(Counts::default());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Arc::new(russh::server::Config { keys: vec![key], auth_rejection_time: std::time::Duration::from_millis(1), ..Default::default() });
    let mut server = FakeServer(counts.clone());
    tokio::spawn(async move {
        let _ = server.run_on_socket(config, &listener).await;
    });
    Fake { port, fp, counts }
}

/// Another valid-looking fingerprint: some other machine's key.
fn other_fp() -> String {
    host_key().1
}

fn secret_sent(c: &Counts) -> usize {
    c.password.load(SeqCst) + c.key.load(SeqCst)
}

#[test]
fn fingerprints_are_read_strictly() {
    let (_, fp) = host_key();
    assert_eq!(ssh::clean_host_key(&format!("  {fp} ")).as_deref(), Some(fp.as_str()));
    for bad in ["", "any", "*", "SHA256:", "SHA256:short", "MD5:aa:bb", &format!("{fp}x"), &fp[1..], &fp.replace("SHA256:", "sha256:")] {
        assert!(ssh::clean_host_key(bad).is_none(), "{bad:?}");
    }
}

/// The pin is never empty and never "any": what the owner confirmed, the
/// saved key for the same VPS, or a new key named with `trust_new`.
#[test]
fn the_pin_is_the_confirmed_key_never_empty() {
    let (a, b) = (other_fp(), other_fp());
    let none = super::Login::default();
    let saved = super::Login { host: "203.0.113.10".into(), port: 22, host_key: a.clone(), ..Default::default() };
    // First contact: only a confirmed key.
    assert!(super::pin_for(&none, "203.0.113.10", 22, "", false).is_err());
    assert_eq!(super::pin_for(&none, "203.0.113.10", 22, &a, false).unwrap(), a);
    assert!(super::pin_for(&none, "203.0.113.10", 22, "not a key", false).is_err());
    // "Trust the new key" alone is not enough: the key must be named.
    assert!(super::pin_for(&saved, "203.0.113.10", 22, "", true).is_err());
    assert!(super::pin_for(&none, "203.0.113.10", 22, "", true).is_err());
    // The same VPS again: the saved key, and a different confirmed key is refused.
    assert_eq!(super::pin_for(&saved, "203.0.113.10", 22, "", false).unwrap(), a);
    assert_eq!(super::pin_for(&saved, "203.0.113.10", 22, &a, false).unwrap(), a);
    let e = super::pin_for(&saved, "203.0.113.10", 22, &b, false).unwrap_err();
    assert!(e.contains("host key changed"), "{e}");
    // Named explicitly, the new key replaces it.
    assert_eq!(super::pin_for(&saved, "203.0.113.10", 22, &b, true).unwrap(), b);
    // Another address or port: nothing is carried over from the saved one.
    assert!(super::pin_for(&saved, "203.0.113.11", 22, "", false).is_err());
    assert!(super::pin_for(&saved, "203.0.113.10", 2222, "", false).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_the_host_key_offers_no_sign_in() {
    let vps = fake_vps().await;
    let seen = Ssh::host_key_of("127.0.0.1", vps.port).await.unwrap();
    assert_eq!(seen, vps.fp);
    // A password sent by the read would be counted before this one is.
    Ssh::connect("127.0.0.1", vps.port, "ubuntu", Cred::Password("pw".into()), &vps.fp).await.unwrap();
    assert_eq!(vps.counts.password.load(SeqCst), 1, "only the confirmed sign-in sent a password");
    assert_eq!(vps.counts.key.load(SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_or_missing_pin_sends_no_password() {
    let vps = fake_vps().await;
    let wrong = other_fp();
    let e = Ssh::connect("127.0.0.1", vps.port, "ubuntu", Cred::Password("pw".into()), &wrong).await.err().expect("refused");
    assert!(e.contains("host key changed") && e.contains(&vps.fp), "{e}");
    // No pin at all is not "learn it": it is refused before connecting.
    let e = Ssh::connect("127.0.0.1", vps.port, "ubuntu", Cred::Password("pw".into()), "  ").await.err().expect("refused");
    assert!(e.contains("host key"), "{e}");
    let (private, _) = ssh::new_key().unwrap();
    assert!(Ssh::connect("127.0.0.1", vps.port, "ubuntu", Cred::Key(ssh::decode(&private).unwrap()), &wrong).await.is_err());
    // The right key goes through, and it is the only sign-in the VPS saw.
    let s = Ssh::connect("127.0.0.1", vps.port, "ubuntu", Cred::Password("pw".into()), &vps.fp).await.unwrap();
    assert_eq!(s.host_key, vps.fp);
    assert_eq!((vps.counts.password.load(SeqCst), vps.counts.key.load(SeqCst)), (1, 0));
}

fn owner_post<'a>(path: &str, cookie: &str, b: Value) -> crate::cross_origin_tests::Call<'a> {
    call("POST", format!("/api/v1/vps-relay/setup/{path}")).origin(OWN).h("cookie", format!("hopperd_session={cookie}")).h("x-hopperd", "1").json(b)
}

fn panel() -> T {
    panel_with(crate::TlsInfo { fingerprint: "AA:BB".into(), ca_pem: String::new() })
}

fn msg(b: &[u8]) -> String {
    body(b)["error"].as_str().map(str::to_string).unwrap_or_else(|| String::from_utf8_lossy(b).into_owned())
}

/// The whole path through the API: nothing is sent until the owner has
/// named the key the VPS showed, and a different key is refused.
#[tokio::test(flavor = "multi_thread")]
async fn connecting_needs_the_confirmed_fingerprint_and_sends_nothing_before() {
    let vps = fake_vps().await;
    let t = panel();
    let me = cookie_login(&t).await;
    let port = vps.port;
    let pw = |extra: Value| {
        let mut b = json!({ "host": "127.0.0.1", "port": port, "user": "ubuntu", "mode": "password", "password": "hunter2" });
        b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        b
    };

    // Not the owner (no session): refused.
    let (s, _, _) = call("POST", "/api/v1/vps-relay/setup/host-key").origin(OWN).h("x-hopperd", "1").json(json!({ "host": "127.0.0.1", "port": port })).send(&t).await;
    assert!(s == StatusCode::UNAUTHORIZED || s == StatusCode::FORBIDDEN, "{s}");

    // Step one reads the key and sends nothing.
    let (s, _, b) = owner_post("host-key", &me, json!({ "host": "127.0.0.1", "port": port })).send(&t).await;
    assert_eq!(s, StatusCode::OK, "{}", msg(&b));
    assert_eq!(body(&b)["host_key"], vps.fp);
    assert_eq!(body(&b)["known"], "");
    assert_eq!(secret_sent(&vps.counts), 0);

    // No fingerprint, "any", a wrong one: each refused, in every mode, with nothing sent.
    for extra in [json!({}), json!({ "host_key": "any" }), json!({ "host_key": "" , "trust_new": true }), json!({ "host_key": other_fp() }), json!({ "host_key": other_fp(), "trust_new": true })] {
        let (s, _, b) = owner_post("connect", &me, pw(extra.clone())).send(&t).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{extra}: {}", msg(&b));
    }
    let (private, _) = ssh::new_key().unwrap();
    let key_body = |extra: Value| {
        let mut b = json!({ "host": "127.0.0.1", "port": port, "user": "ubuntu", "mode": "key", "key": private });
        b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        b
    };
    for extra in [json!({}), json!({ "host_key": other_fp() })] {
        let (s, _, _) = owner_post("connect", &me, key_body(extra)).send(&t).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }
    assert_eq!(secret_sent(&vps.counts), 0, "no password or key reached the VPS");
    assert!(t.app.vps_setup.login().host.is_empty(), "nothing saved");

    // The key the owner confirmed: the password goes out once, then the new key.
    let (s, _, b) = owner_post("connect", &me, pw(json!({ "host_key": vps.fp }))).send(&t).await;
    assert_eq!(s, StatusCode::OK, "{}", msg(&b));
    assert_eq!(body(&b)["login"]["host_key"], vps.fp);
    assert_eq!(vps.counts.password.load(SeqCst), 1);
    assert_eq!(vps.counts.key.load(SeqCst), 1);
    assert_eq!(t.app.vps_setup.login().host_key, vps.fp);

    // The same VPS again needs no fingerprint (the saved one is used); a
    // different one is refused unless it is named with trust_new.
    let saved = t.app.vps_setup.login();
    let key_body = |extra: Value| {
        let mut b = json!({ "host": "127.0.0.1", "port": port, "user": "ubuntu", "mode": "key", "key": saved.private_key });
        b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        b
    };
    let (s, _, b) = owner_post("connect", &me, key_body(json!({}))).send(&t).await;
    assert_eq!(s, StatusCode::OK, "{}", msg(&b));
    let (s, _, b) = owner_post("connect", &me, key_body(json!({ "host_key": other_fp() }))).send(&t).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(msg(&b).contains("host key changed"), "{}", msg(&b));
}

/// A reinstalled VPS shows a new key: it is only taken when the owner names
/// it, and a key that isn't what the VPS shows still sends nothing.
#[tokio::test(flavor = "multi_thread")]
async fn trust_new_takes_only_the_key_named_and_checked() {
    let vps = fake_vps().await;
    let t = panel();
    let me = cookie_login(&t).await;
    let port = vps.port;
    let (private, public) = ssh::new_key().unwrap();
    // A login saved earlier, with the key the VPS had before it was reinstalled.
    let old = other_fp();
    t.app.vps_setup.save_login(super::Login { host: "127.0.0.1".into(), port, user: "ubuntu".into(), host_key: old.clone(), private_key: private.clone(), public_key: public, root: true, ..Default::default() }).unwrap();
    let body_with = |extra: Value| {
        let mut b = json!({ "host": "127.0.0.1", "port": port, "user": "ubuntu", "mode": "key", "key": private });
        b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        b
    };

    // The saved key no longer matches what the VPS shows: refused, nothing sent.
    let (s, _, b) = owner_post("connect", &me, body_with(json!({}))).send(&t).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(msg(&b).contains("host key changed") && msg(&b).contains(&vps.fp), "{}", msg(&b));
    // The box alone (no key named) is not enough, and "any" is no key.
    for extra in [json!({ "trust_new": true }), json!({ "trust_new": true, "host_key": "any" })] {
        let (s, _, _) = owner_post("connect", &me, body_with(extra)).send(&t).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }
    // A key named that is not the one the VPS shows: still nothing sent.
    let (s, _, _) = owner_post("connect", &me, body_with(json!({ "trust_new": true, "host_key": other_fp() }))).send(&t).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(secret_sent(&vps.counts), 0);
    assert_eq!(t.app.vps_setup.login().host_key, old, "the saved key stays until a new one is confirmed");

    // The key the VPS shows, named on purpose: taken and saved.
    let (s, _, b) = owner_post("connect", &me, body_with(json!({ "trust_new": true, "host_key": vps.fp }))).send(&t).await;
    assert_eq!(s, StatusCode::OK, "{}", msg(&b));
    assert_eq!(t.app.vps_setup.login().host_key, vps.fp);
    // The read now says which key was known.
    let (_, _, b) = owner_post("host-key", &me, json!({ "host": "127.0.0.1", "port": port })).send(&t).await;
    assert_eq!(body(&b)["known"], vps.fp);
}

/// Not a test: a stand-in VPS for looking at the setup dialog in a browser
/// (`cargo test -p hopperd fake_vps_for_the_browser -- --ignored --nocapture`
/// with `FAKE_VPS_PORT`). It prints its host key and every sign-in it is offered.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs until stopped; start it by hand"]
async fn fake_vps_for_the_browser() {
    let port = std::env::var("FAKE_VPS_PORT").ok().and_then(|p| p.parse().ok()).expect("FAKE_VPS_PORT");
    let vps = fake_vps_on(port).await;
    println!("fake vps on 127.0.0.1:{port} host key {}", vps.fp);
    let mut last = (0, 0);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let now = (vps.counts.password.load(SeqCst), vps.counts.key.load(SeqCst));
        if now != last {
            println!("sign-ins offered: {} password, {} key", now.0, now.1);
            last = now;
        }
    }
}
