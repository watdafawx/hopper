//! Desktop pairing through the real router (security review H4 in
//! design/multi-panel.md): commit and reveal, what an older Hopper is told,
//! and the real desktop client pairing over TLS.

use crate::cross_origin_tests::{OWN, T, body, call, cookie_login, panel_with};
use axum::http::StatusCode;
use serde_json::{Value, json};

fn panel() -> (T, crate::tls::Ca) {
    let dir = std::env::temp_dir().join(format!("hopperd-pair-ca-{}-{}", std::process::id(), crate::auth::random_hex(4)));
    std::fs::create_dir_all(&dir).unwrap();
    let ca = crate::tls::load_or_create_ca(&dir, "panel").unwrap();
    let t = panel_with(crate::TlsInfo { fingerprint: crate::tls::fingerprint(&ca.der), ca_pem: ca.pem.clone() });
    let _ = std::fs::remove_dir_all(&dir);
    (t, ca)
}

async fn pending(t: &T, me: &str) -> Vec<Value> {
    let (s, _, b) = call("GET", "/api/v1/pair/pending").origin(OWN).h("cookie", format!("hopperd_session={me}")).h("x-hopperd", "1").send(t).await;
    assert_eq!(s, StatusCode::OK);
    body(&b).as_array().unwrap().clone()
}

/// A Hopper from before commit and reveal sends no commitment: the panel
/// refuses it with words that say what to do, and shows nothing to allow.
#[tokio::test]
async fn an_older_hopper_is_told_to_update_it() {
    let (t, _) = panel();
    let me = cookie_login(&t).await;
    let (s, _, b) = call("POST", "/api/v1/pair/request").from("192.168.50.21").json(json!({ "device": "Hopper on FOX" })).send(&t).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(body(&b)["error"], hopper_core::hopperd::OLD_HOPPER);
    assert!(pending(&t, &me).await.is_empty());
    // The panel says it pairs this way, so a new Hopper knows before asking.
    let (_, _, b) = call("GET", "/api/v1/hello").send(&t).await;
    assert!(body(&b)["capabilities"].as_array().unwrap().iter().any(|c| c == hopper_core::hopperd::PAIR_COMMIT));
}

/// The owner sees (and can allow) a request only after the PC revealed the
/// nonce it committed to, and the code covers that nonce.
#[tokio::test]
async fn the_panel_shows_a_request_only_after_the_reveal() {
    use hopper_core::hopperd::{committed_pair_code, pair_commit, pair_nonce};
    let (t, _) = panel();
    let me = cookie_login(&t).await;
    let nonce = pair_nonce();
    let ask = |commit: String| call("POST", "/api/v1/pair/request").from("192.168.50.21").json(json!({ "device": "Hopper on FOX", "commit": commit }));
    let r = body(&ask(pair_commit(&nonce)).send(&t).await.2);
    let (id, secret) = (r["id"].as_str().unwrap().to_string(), r["secret"].as_str().unwrap().to_string());
    assert!(r.get("code").is_none(), "the code is never sent");
    assert!(pending(&t, &me).await.is_empty(), "no code shown before the reveal");
    let answer = |id: &str| {
        call("POST", format!("/api/v1/pair/{id}/answer")).origin(OWN).h("cookie", format!("hopperd_session={me}")).h("x-hopperd", "1").json(json!({ "allow": true }))
    };
    assert_eq!(answer(&id).send(&t).await.0, StatusCode::BAD_REQUEST, "nothing to allow yet");
    let status = |id: &str, secret: &str, nonce: &str| call("POST", "/api/v1/pair/status").json(json!({ "id": id, "secret": secret, "nonce": nonce }));
    assert_eq!(body(&status(&id, &secret, &nonce).send(&t).await.2)["status"], "waiting");
    let p = pending(&t, &me).await;
    assert_eq!(p[0]["code"].as_str().unwrap(), committed_pair_code(&t.app.tls.fingerprint, &id, &secret, &nonce));
    assert_eq!(answer(&id).send(&t).await.0, StatusCode::OK);
    assert_eq!(body(&status(&id, &secret, "").send(&t).await.2)["status"], "approved");

    // A reveal that doesn't match ends the request.
    let r = body(&ask(pair_commit(&pair_nonce())).send(&t).await.2);
    let (id2, s2) = (r["id"].as_str().unwrap().to_string(), r["secret"].as_str().unwrap().to_string());
    assert_eq!(body(&status(&id2, &s2, &nonce).send(&t).await.2)["status"], "unknown");
    assert!(pending(&t, &me).await.is_empty());
}

/// The real desktop client (`hopper_core::hopperd::pair_request`) pairs
/// with the real panel over TLS: its code is the one the owner sees, and the
/// token it gets works.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_hopper_pairs_with_a_new_panel() {
    let (t, ca) = panel();
    let me = cookie_login(&t).await;
    let addr = serve_tls(&t, &ca).await;
    let url = format!("https://127.0.0.1:{}", addr.port());
    let p = tokio::task::spawn_blocking(move || hopper_core::hopperd::pair_request(&url, "Hopper on FOX")).await.unwrap().unwrap();
    let shown = pending(&t, &me).await;
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0]["code"].as_str().unwrap(), p.code, "both sides show the same code");
    let id = shown[0]["id"].as_str().unwrap().to_string();
    let (s, _, _) = call("POST", format!("/api/v1/pair/{id}/answer")).origin(OWN).h("cookie", format!("hopperd_session={me}")).h("x-hopperd", "1").json(json!({ "allow": true })).send(&t).await;
    assert_eq!(s, StatusCode::OK);
    let st = tokio::task::spawn_blocking(move || hopper_core::hopperd::pair_status(&p)).await.unwrap().unwrap();
    let hopper_core::hopperd::PairState::Approved(token) = st else { panic!("{st:?}") };
    assert_eq!(call("GET", "/api/v1/servers").bearer(&token).send(&t).await.0, StatusCode::OK);
}

async fn serve_tls(t: &T, ca: &crate::tls::Ca) -> std::net::SocketAddr {
    let own: crate::tls::OwnCert = std::sync::Arc::new(std::sync::RwLock::new(std::sync::Arc::new(
        crate::tls::issue(ca, &["localhost".into()], &["127.0.0.1".parse().unwrap()]).unwrap(),
    )));
    let resolver = |internet| crate::tls::Resolver { own: own.clone(), acme: t.app.acme.clone(), internet };
    let tls = crate::net::Tls { lan: crate::tls::server_config(resolver(false)), internet: crate::tls::server_config(resolver(true)) };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = crate::api::router(t.app.clone());
    tokio::spawn(crate::net::serve(listener, router, tls, std::future::pending()));
    addr
}
