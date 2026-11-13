//! Passwords in server.properties through the real router
//! (design/server-secrets.md): who gets them, who doesn't, and that
//! saving without them keeps them. Also per-server access for a user
//! limited to some servers.

use crate::auth::Role;
use crate::cross_origin_tests::{OWN, PASSWORD, T, body, call, panel_with};
use axum::http::StatusCode;
use serde_json::{Value, json};

const PROPS: &str = "motd=test\nenable-rcon=true\nrcon.port=25575\nrcon.password=hunter22\nmanagement-server-secret=s3cretkey\nmax-players=12\n";

fn panel() -> T {
    let t = panel_with(crate::TlsInfo { fingerprint: "AA:BB".into(), ca_pem: "-----BEGIN CERTIFICATE-----\nTEST\n-----END CERTIFICATE-----\n".into() });
    let srv = t.dir.join("servers").join("Test Server");
    std::fs::write(srv.join("server.properties"), PROPS).unwrap();
    // A second server for the limited user's tests.
    let other = t.dir.join("servers").join("Other Server");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("server.properties"), PROPS).unwrap();
    t.app.rescan();
    t
}

fn other_id(t: &T) -> String {
    t.app.servers.list().iter().find(|e| e.id != t.server).expect("the other server").id.clone()
}

/// Someone's session cookie, signed in on the panel's own page.
async fn sign_in(t: &T, name: Option<&str>, password: &str) -> String {
    let b = match name {
        Some(n) => json!({ "name": n, "password": password }),
        None => json!({ "password": password }),
    };
    let (s, h, raw) = call("POST", "/api/v1/auth/login").origin(OWN).json(b).send(t).await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&raw));
    let c = h["set-cookie"].to_str().unwrap();
    c.split(';').next().unwrap().strip_prefix("hopperd_session=").unwrap().to_string()
}

enum As<'a> {
    Cookie(&'a str),
    Token(&'a str),
}

async fn send(t: &T, who: &As<'_>, method: &'static str, path: &str, b: Option<Value>) -> (StatusCode, Vec<u8>) {
    let mut c = call(method, path);
    c = match who {
        As::Cookie(k) => c.origin(OWN).h("cookie", format!("hopperd_session={k}")).h("x-hopperd", "1"),
        As::Token(k) => c.bearer(k),
    };
    if let Some(b) = b {
        c = c.json(b);
    }
    let (s, _, raw) = c.send(t).await;
    (s, raw)
}

fn has_secret(raw: &[u8]) -> bool {
    let s = String::from_utf8_lossy(raw);
    s.contains("hunter22") || s.contains("s3cretkey")
}

fn zip_names(raw: &[u8]) -> Vec<String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(raw.to_vec())).expect("a zip");
    (0..z.len()).map(|i| z.by_index(i).unwrap().name().to_string()).collect()
}

/// Everything someone without file rights tries: never a secret, the rest
/// of the settings still there.
async fn gets_no_secret(t: &T, who: As<'_>, label: &str) {
    let id = &t.server;
    let (s, raw) = send(t, &who, "GET", "/api/v1/servers/infos", None).await;
    assert_eq!(s, StatusCode::OK, "{label}");
    assert!(!has_secret(&raw), "{label}: infos");
    let v = body(&raw);
    let me = v.as_array().unwrap().iter().find(|s| s["id"] == id.as_str()).unwrap();
    assert_eq!(me["properties"]["max-players"], "12", "{label}: the rest stays");

    let (s, raw) = send(t, &who, "GET", &format!("/api/v1/servers/{id}/properties"), None).await;
    assert_eq!(s, StatusCode::OK, "{label}");
    assert!(!has_secret(&raw), "{label}: properties");
    let v = body(&raw);
    assert!(v["values"].get("rcon.password").is_none() && v["values"]["rcon.port"] == "25575", "{label}: {v}");

    for p in [
        "fs/{}/text?path=server.properties",
        "fs/{}/file?path=server.properties",
        "fs/{}/file?path=server.properties.",
        "fs/{}/file?path=.%2FSERVER.PROPERTIES",
        "fs/{}/diff?path=server.properties",
        "fs/{}/history?path=server.properties",
        "fs/{}/list?path=.hopper-trash",
        "fs/{}/zip?path=server.properties",
        "fs/{}/zip?path=.hopper-trash",
    ] {
        let (s, raw) = send(t, &who, "GET", &format!("/api/v1/{}", p.replace("{}", id)), None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{label}: {p}");
        assert!(!has_secret(&raw), "{label}: {p}");
    }
    // A search inside files leaves server.properties out.
    let (s, raw) = send(t, &who, "GET", &format!("/api/v1/fs/{id}/search?path=&q=hunter22"), None).await;
    assert_eq!(s, StatusCode::OK, "{label}");
    assert!(body(&raw)["hits"].as_array().unwrap().is_empty(), "{label}: search");
    // A zip of the whole server leaves it out; other files stay in.
    let (s, raw) = send(t, &who, "GET", &format!("/api/v1/fs/{id}/zip"), None).await;
    assert_eq!(s, StatusCode::OK, "{label}");
    let names = zip_names(&raw);
    assert!(names.iter().any(|n| n == "hello.txt"), "{label}: {names:?}");
    assert!(!names.iter().any(|n| n.to_lowercase().contains("server.properties") || n.contains(".hopper")), "{label}: {names:?}");
    // Ordinary files still open.
    let (s, _) = send(t, &who, "GET", &format!("/api/v1/fs/{id}/text?path=hello.txt"), None).await;
    assert_eq!(s, StatusCode::OK, "{label}");
}

/// The owner saves server.properties once, so a saved-over copy with the
/// password sits in the trash.
async fn with_saved_over_copy(t: &T, owner: &str) {
    let (s, _) = send(t, &As::Cookie(owner), "PUT", &format!("/api/v1/fs/{}/text", t.server), Some(json!({ "path": "server.properties", "content": PROPS, "force": true }))).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn a_viewer_gets_no_secret() {
    let t = panel();
    let owner = sign_in(&t, None, PASSWORD).await;
    with_saved_over_copy(&t, &owner).await;
    t.app.auth.user_create("vic", "vicpass12", Role::Viewer, vec![]).unwrap();
    let vic = sign_in(&t, Some("vic"), "vicpass12").await;
    gets_no_secret(&t, As::Cookie(&vic), "viewer").await;
    // The saved-over copy can't be read by its id either.
    let (_, raw) = send(&t, &As::Cookie(&owner), "GET", &format!("/api/v1/fs/{}/history?path=server.properties", t.server), None).await;
    let version = body(&raw)["versions"][0]["id"].as_str().expect("a saved-over copy").to_string();
    let (s, raw) = send(&t, &As::Cookie(&vic), "GET", &format!("/api/v1/fs/{}/version-text?version={version}", t.server), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(!has_secret(&raw));
    // An operator is the same.
    t.app.auth.user_create("op", "oppass123", Role::Operator, vec![]).unwrap();
    let op = sign_in(&t, Some("op"), "oppass123").await;
    gets_no_secret(&t, As::Cookie(&op), "operator").await;
}

#[tokio::test]
async fn a_read_only_token_gets_no_secret() {
    let t = panel();
    let (tok, _) = t.app.auth.token_create("grafana", true, vec![], None).unwrap();
    gets_no_secret(&t, As::Token(&tok), "read-only token").await;
}

/// Those who may change the server's files see the password (they could
/// set a new one anyway), and saving without it keeps it.
#[tokio::test]
async fn an_admin_sees_it_and_saving_without_it_keeps_it() {
    let t = panel();
    t.app.auth.user_create("sam", "sampass12", Role::Admin, vec![]).unwrap();
    let sam = sign_in(&t, Some("sam"), "sampass12").await;
    let id = t.server.clone();
    let (s, raw) = send(&t, &As::Cookie(&sam), "GET", &format!("/api/v1/servers/{id}/properties"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body(&raw)["values"]["rcon.password"], "hunter22");
    let (s, raw) = send(&t, &As::Cookie(&sam), "GET", &format!("/api/v1/fs/{id}/text?path=server.properties"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(has_secret(&raw));
    // /servers/infos never carries it, for anyone: Hopper desktop reads
    // players, the world and the message there.
    let (_, raw) = send(&t, &As::Cookie(&sam), "GET", "/api/v1/servers/infos", None).await;
    assert!(!has_secret(&raw));
    // Saving other keys keeps the passwords.
    let (s, raw) = send(&t, &As::Cookie(&sam), "PUT", &format!("/api/v1/servers/{id}/properties"), Some(json!({ "values": { "max-players": "20" } }))).await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&raw));
    let file = std::fs::read_to_string(t.dir.join("servers").join("Test Server").join("server.properties")).unwrap();
    assert!(file.contains("max-players=20") && file.contains("rcon.password=hunter22") && file.contains("management-server-secret=s3cretkey"), "{file}");
    // A viewer can't save at all.
    t.app.auth.user_create("vic", "vicpass12", Role::Viewer, vec![]).unwrap();
    let vic = sign_in(&t, Some("vic"), "vicpass12").await;
    let (s, _) = send(&t, &As::Cookie(&vic), "PUT", &format!("/api/v1/servers/{id}/properties"), Some(json!({ "values": { "rcon.password": "" } }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // A token that may write is file rights too.
    let (tok, _) = t.app.auth.token_create("script", false, vec![], None).unwrap();
    let (_, raw) = send(&t, &As::Token(&tok), "GET", &format!("/api/v1/servers/{id}/properties"), None).await;
    assert_eq!(body(&raw)["values"]["rcon.password"], "hunter22");
}

/// A user with some servers reaches none of the others' routes, reading
/// or writing; an Admin limited to one server is still an Admin there.
#[tokio::test]
async fn a_limited_user_cant_reach_other_servers() {
    let t = panel();
    let (mine, other) = (t.server.clone(), other_id(&t));
    t.app.auth.user_create("lim", "limpass12", Role::Admin, vec![mine.clone()]).unwrap();
    let lim = sign_in(&t, Some("lim"), "limpass12").await;
    let who = As::Cookie(&lim);
    for (m, p, b) in [
        ("GET", format!("/api/v1/servers/{other}/properties"), None),
        ("PUT", format!("/api/v1/servers/{other}/properties"), Some(json!({ "values": { "motd": "x" } }))),
        ("GET", format!("/api/v1/servers/{other}/jvm"), None),
        ("GET", format!("/api/v1/servers/{other}"), None),
        ("GET", format!("/api/v1/servers/{other}/backups"), None),
        ("GET", format!("/api/v1/fs/{other}/list?path="), None),
        ("GET", format!("/api/v1/fs/{other}/text?path=hello.txt"), None),
        ("PUT", format!("/api/v1/fs/{other}/text"), Some(json!({ "path": "x.txt", "content": "x", "force": true }))),
        ("GET", format!("/api/v1/fs/{other}/zip-list?path=a.zip"), None),
        ("GET", format!("/api/v1/fs/{other}/version-text?version=x"), None),
        ("POST", format!("/api/v1/fs/{other}/link"), Some(json!({ "path": "server.properties" }))),
    ] {
        let (s, raw) = send(&t, &who, m, &p, b).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p}: {}", String::from_utf8_lossy(&raw));
    }
    // Encoded ids are the same server.
    let (s, _) = send(&t, &who, "GET", &format!("/api/v1/servers/{}/properties", other.replace('-', "%2D")), None).await;
    assert_ne!(s, StatusCode::OK);
    // The server list and infos leave the other out.
    let (_, raw) = send(&t, &who, "GET", "/api/v1/servers/infos", None).await;
    let ids: Vec<String> = body(&raw).as_array().unwrap().iter().filter_map(|s| s["id"].as_str().map(str::to_string)).collect();
    assert_eq!(ids, vec![mine.clone()]);
    // Their own server works, password included (Admin there).
    let (s, raw) = send(&t, &who, "GET", &format!("/api/v1/servers/{mine}/properties"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body(&raw)["values"]["rcon.password"], "hunter22");
    // The same with a token scoped to one server.
    let (tok, _) = t.app.auth.token_create("one", false, vec![mine.clone()], None).unwrap();
    let (s, _) = send(&t, &As::Token(&tok), "GET", &format!("/api/v1/servers/{other}/properties"), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

/// A one-use link doesn't carry what the route wouldn't.
#[tokio::test]
async fn a_link_carries_no_secret() {
    let t = panel();
    t.app.auth.user_create("vic", "vicpass12", Role::Viewer, vec![]).unwrap();
    let vic = sign_in(&t, Some("vic"), "vicpass12").await;
    let id = t.server.clone();
    for b in [json!({ "path": "server.properties" }), json!({ "paths": ["server.properties"], "zip": true })] {
        let (s, raw) = send(&t, &As::Cookie(&vic), "POST", &format!("/api/v1/fs/{id}/link"), Some(b.clone())).await;
        assert_eq!(s, StatusCode::OK, "asking is a read");
        let url = body(&raw)["url"].as_str().unwrap().to_string();
        let (s, _, raw) = call("GET", url).origin(OWN).h("cookie", format!("hopperd_session={vic}")).send(&t).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
        assert!(!has_secret(&raw));
    }
}
