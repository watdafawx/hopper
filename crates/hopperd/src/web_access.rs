//! Other web pages that may use this panel (design/multi-panel.md, B0).
//!
//! A web UI served by one panel can call another panel's API directly. The
//! browser only lets it read the answers when the called panel says so with
//! CORS headers, and this panel says so only for the exact origins its
//! owner listed (`web_origins` in hopperd.toml). Empty, as by default, means
//! no CORS headers at all: the panel behaves as it did before.
//!
//! What this never does: answer `*`, send `Access-Control-Allow-Credentials`
//! (an allowed page can't ride the person's cookie; it needs a bearer
//! token), or let another page manage who can get in unless the owner
//! turned that on. The list and that switch only change on the panel's own
//! page.

use crate::App;
use crate::api::{ApiError, ApiResult, bad, require_owner_who, token};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Most pages a panel lists; more is a mistake, not a setup.
const MAX_ORIGINS: usize = 32;
/// Longest origin accepted (a long DNS name and a port fit easily).
const MAX_LEN: usize = 200;

const ALLOW_METHODS: &str = "GET, HEAD, POST, PUT, DELETE";
const ALLOW_HEADERS: &str = "authorization, content-type, x-hopperd, x-hopperd-client";
const EXPOSE_HEADERS: &str = "content-disposition, content-length";

/// The settings route: its list and switch only change from the panel's own page.
pub const SETTINGS_PATH: &str = "/api/v1/settings/web-access";

/// An origin as browsers send it in `Origin`: `scheme://host[:port]`,
/// lower case, no default port, no path. Errors are shown to the owner.
pub fn normalize(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("Type the page's address, like http://fox:8790.".into());
    }
    if s.len() > MAX_LEN {
        return Err(format!("“{}…” is too long for a web address.", s.chars().take(40).collect::<String>()));
    }
    if s.contains('*') {
        return Err(format!("“{s}”: wildcards aren't allowed. Add each page's exact address."));
    }
    let lower = s.to_ascii_lowercase();
    let (scheme, rest) = lower
        .split_once("://")
        .ok_or_else(|| format!("“{s}” needs http:// or https:// in front."))?;
    if scheme != "http" && scheme != "https" {
        return Err(format!("“{s}” must start with http:// or https://."));
    }
    // People paste `http://fox:8790/`; the slash alone is fine, a real path isn't.
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains(['/', '?', '#', '\\']) {
        return Err(format!("“{s}”: only the address and port, without a path."));
    }
    if rest.contains('@') {
        return Err(format!("“{s}”: no name or password in the address."));
    }
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let (h, after) = v6.split_once(']').ok_or_else(|| format!("“{s}” isn't a valid address."))?;
        if h.is_empty() || !h.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.') {
            return Err(format!("“{s}” isn't a valid address."));
        }
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':').ok_or_else(|| format!("“{s}” isn't a valid address."))?),
        };
        (format!("[{h}]"), port)
    } else {
        let (h, p) = match rest.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (rest, None),
        };
        let label_ok = |l: &str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if h.is_empty() || !h.split('.').all(label_ok) {
            return Err(format!("“{s}” isn't a valid address."));
        }
        (h.to_string(), p)
    };
    let port = match port {
        None => None,
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 && p.chars().all(|c| c.is_ascii_digit()) => Some(n),
            _ => return Err(format!("“{s}”: the port must be a number from 1 to 65535.")),
        },
    };
    // Browsers leave the default port out of `Origin`, so the list must too.
    let port = port.filter(|&p| !((scheme == "http" && p == 80) || (scheme == "https" && p == 443)));
    Ok(match port {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    })
}

/// Checks and tidies a whole list: every entry valid, no repeats, not too many.
pub fn normalize_all(list: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for o in list.iter().filter(|o| !o.trim().is_empty()) {
        let n = normalize(o)?;
        if !out.contains(&n) {
            out.push(n);
        }
    }
    if out.len() > MAX_ORIGINS {
        return Err(format!("At most {MAX_ORIGINS} pages."));
    }
    Ok(out)
}

/// The request's `Origin`, when it sends a usable one.
fn origin(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).filter(|o| !o.is_empty() && *o != "null")
}

/// Whether `Origin` names this panel itself (its host and port match `Host`),
/// the same test the live WebSocket uses for cookies.
pub fn is_own(headers: &HeaderMap) -> bool {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let Some(o) = origin(headers) else { return false };
    let o_host = o.split_once("://").map_or("", |(_, h)| h);
    !host.is_empty() && o_host.eq_ignore_ascii_case(host)
}

/// A page on another origin that the owner listed: the origin, else None.
/// The panel's own page is never "another" origin, even if listed.
pub fn listed_other<'a>(headers: &'a HeaderMap, list: &[String]) -> Option<&'a str> {
    let o = origin(headers)?;
    (!is_own(headers) && list.iter().any(|l| l == o)).then_some(o)
}

/// Changes that decide who can get in: users, devices and pairing, API
/// tokens, passwords and two-factor, the certificate, and this list.
fn manages_access(path: &str) -> bool {
    let under = |p: &str| path == p || path.strip_prefix(p).is_some_and(|r| r.starts_with('/'));
    under("/api/v1/users")
        || under("/api/v1/tokens")
        || under("/api/v1/devices")
        || (path.starts_with("/api/v1/pair/") && path.ends_with("/answer"))
        || under("/api/v1/auth/2fa")
        || path == "/api/v1/auth/password"
        || path == "/api/v1/settings/certificate"
        || path == SETTINGS_PATH
}

/// The one check for "who can get in" from other pages. `Some(message)`
/// refuses the request. Reads stay open; only changes are checked.
/// - This list and its switch: only from the panel's own page, always.
/// - The rest: refused from a listed other page unless the owner turned on
///   "Let other web pages manage who can get in".
pub fn access_refusal(headers: &HeaderMap, list: &[String], manage_from_others: bool, method: &Method, path: &str) -> Option<&'static str> {
    if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS || !manages_access(path) {
        return None;
    }
    if path == SETTINGS_PATH {
        return (origin(headers).is_some() && !is_own(headers)).then_some("Change this on the panel's own page.");
    }
    (!manage_from_others && listed_other(headers, list).is_some())
        .then_some("Users, devices, tokens, two-factor and the certificate can only be changed on this panel's own page.")
}

/// What the security middleware does with a request before its checks.
pub enum Before {
    /// Answer straight away (a CORS preflight).
    Answer(Response),
    /// Carry on to the checks and the route.
    Go,
}

/// A CORS preflight (`OPTIONS` with `Access-Control-Request-Method`) gets its
/// answer here without signing in; anything else carries on. A request from
/// a listed page loses its cookie: such a page signs in with a bearer token,
/// never with the person's cookie session on this panel.
pub fn before(req: &mut Request, list: &[String]) -> Before {
    let listed = listed_other(req.headers(), list).map(str::to_string);
    let preflight = req.method() == Method::OPTIONS && req.headers().contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    if preflight && origin(req.headers()).is_some() {
        let pna = req
            .headers()
            .get("access-control-request-private-network")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("true"));
        let mut res = match &listed {
            Some(_) => StatusCode::NO_CONTENT.into_response(),
            None => ApiError(StatusCode::FORBIDDEN, "This web page may not use this panel. Its owner can add it in Settings › Access & devices.".into()).into_response(),
        };
        let h = res.headers_mut();
        h.append(header::VARY, HeaderValue::from_static("origin"));
        if let Some(o) = &listed {
            add_allow(h, o);
            h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static(ALLOW_METHODS));
            h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static(ALLOW_HEADERS));
            h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
            // Chrome's Private Network Access: a public page asking a LAN panel.
            if pna {
                h.insert("access-control-allow-private-network", HeaderValue::from_static("true"));
            }
        }
        return Before::Answer(res);
    }
    if listed.is_some() {
        req.headers_mut().remove(header::COOKIE);
    }
    Before::Go
}

fn add_allow(h: &mut HeaderMap, origin: &str) {
    if let Ok(v) = HeaderValue::from_str(origin) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
    }
}

/// Adds the CORS headers to an answer for a listed page, and `Vary: Origin`
/// to every API answer to a request that sent an `Origin` (caches must not
/// hand one page's answer to another).
pub fn after(mut res: Response, listed: Option<&str>, had_origin: bool) -> Response {
    let h = res.headers_mut();
    if had_origin || listed.is_some() {
        h.append(header::VARY, HeaderValue::from_static("origin"));
    }
    if let Some(o) = listed {
        add_allow(h, o);
        h.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, HeaderValue::from_static(EXPOSE_HEADERS));
    }
    res
}

/// Whether a request sent an `Origin` at all.
pub fn has_origin(headers: &HeaderMap) -> bool {
    origin(headers).is_some()
}

// ---- the setting ----------------------------------------------------------------

pub fn routes() -> Router<Arc<App>> {
    Router::new().route(SETTINGS_PATH, get(get_settings).put(put_settings))
}

fn view(app: &App) -> Value {
    let cfg = app.cfg.read().unwrap();
    json!({ "origins": cfg.web_origins, "manage": cfg.web_manage_access })
}

async fn get_settings(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner_who(&app, &headers, &method)?;
    Ok(Json(view(&app)))
}

#[derive(Deserialize)]
struct Body {
    origins: Vec<String>,
    #[serde(default)]
    manage: bool,
}

async fn put_settings(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<Body>) -> ApiResult<Response> {
    require_owner_who(&app, &headers, &method)?;
    // The panel's own page: signed in with its cookie, from its own origin.
    // Not a device, a token or another page, whatever the switch says.
    if !token(&headers).1 || !is_own(&headers) {
        return Err(ApiError(StatusCode::FORBIDDEN, "Change this on the panel's own page.".into()));
    }
    let origins = normalize_all(&b.origins).map_err(bad)?;
    let note = {
        let mut cfg = app.cfg.write().unwrap();
        let added: Vec<&String> = origins.iter().filter(|o| !cfg.web_origins.contains(o)).collect();
        let removed: Vec<&String> = cfg.web_origins.iter().filter(|o| !origins.contains(o)).collect();
        let mut parts = Vec::new();
        if !added.is_empty() {
            parts.push(format!("added {}", added.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
        }
        if !removed.is_empty() {
            parts.push(format!("removed {}", removed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
        }
        if cfg.web_manage_access != b.manage {
            parts.push(format!("other pages manage access: {}", if b.manage { "on" } else { "off" }));
        }
        cfg.web_origins = origins;
        cfg.web_manage_access = b.manage;
        cfg.save(&app.data_dir).map_err(|e| bad(format!("{e:#}")))?;
        if parts.is_empty() { "no change".to_string() } else { parts.join("; ") }
    };
    log::info!("web pages allowed to use this panel: {note}");
    let mut r = Json(view(&app)).into_response();
    r.extensions_mut().insert(crate::security::AuditNote(note));
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body as B;

    fn hdrs(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn list() -> Vec<String> {
        vec!["http://fox:8790".into(), "https://panel.example.com".into()]
    }

    #[test]
    fn origins_are_validated_and_tidied() {
        assert_eq!(normalize("http://fox:8790").unwrap(), "http://fox:8790");
        assert_eq!(normalize("  HTTP://Fox:8790/ ").unwrap(), "http://fox:8790");
        assert_eq!(normalize("https://panel.example.com:443").unwrap(), "https://panel.example.com");
        assert_eq!(normalize("http://192.168.50.2:80").unwrap(), "http://192.168.50.2");
        assert_eq!(normalize("http://[::1]:8790").unwrap(), "http://[::1]:8790");
        assert_eq!(normalize("https://my-host.ts.net").unwrap(), "https://my-host.ts.net");
        for bad in [
            "",
            "fox:8790",
            "ftp://fox",
            "http://fox:8790/panel",
            "http://fox:8790?x=1",
            "http://fox#a",
            "http://*.example.com",
            "*",
            "http://user:pw@fox",
            "http://fox:0",
            "http://fox:65536",
            "http://fox:+80",
            "http://",
            "http://-fox",
            "http://fo_x",
            "http://[zz]:1",
            "null",
            "javascript://x",
        ] {
            assert!(normalize(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(normalize(&format!("http://{}.com", "a".repeat(300))).is_err());
    }

    #[test]
    fn list_drops_blanks_and_repeats() {
        let l = normalize_all(&["http://fox:8790".into(), " ".into(), "HTTP://FOX:8790/".into()]).unwrap();
        assert_eq!(l, ["http://fox:8790"]);
        let many: Vec<String> = (1..=MAX_ORIGINS as u16 + 1).map(|p| format!("http://fox:{p}")).collect();
        assert!(normalize_all(&many).is_err());
        assert!(normalize_all(&["http://ok:1".into(), "nope".into()]).is_err(), "one bad entry refuses the list");
    }

    #[test]
    fn own_and_listed_origins() {
        let own = hdrs(&[("host", "fox:8790"), ("origin", "http://fox:8790")]);
        assert!(is_own(&own));
        assert_eq!(listed_other(&own, &list()), None, "the panel's own page is never 'other'");
        let other = hdrs(&[("host", "rig:8791"), ("origin", "http://fox:8790")]);
        assert!(!is_own(&other));
        assert_eq!(listed_other(&other, &list()), Some("http://fox:8790"));
        let unlisted = hdrs(&[("host", "rig:8791"), ("origin", "http://evil:8790")]);
        assert_eq!(listed_other(&unlisted, &list()), None);
        // Exact match only: no prefix, suffix or case games.
        for o in ["http://fox:87901", "http://fox:879", "https://fox:8790", "http://fox:8790.evil.com", "http://FOX:8790"] {
            let h = hdrs(&[("host", "rig:8791"), ("origin", o)]);
            assert_eq!(listed_other(&h, &list()), None, "{o}");
        }
        assert_eq!(listed_other(&hdrs(&[("host", "rig:8791"), ("origin", "null")]), &["null".into()]), None);
        assert_eq!(listed_other(&hdrs(&[("host", "rig:8791")]), &list()), None);
    }

    fn run_before(method: Method, h: &[(&str, &str)], l: &[String]) -> (Before, HeaderMap) {
        let mut b = Request::builder().method(method).uri("/api/v1/servers");
        for (k, v) in h {
            b = b.header(*k, *v);
        }
        let mut req = b.body(B::empty()).unwrap();
        let r = before(&mut req, l);
        (r, req.headers().clone())
    }

    #[test]
    fn preflight_for_listed_origin() {
        let (r, _) = run_before(
            Method::OPTIONS,
            &[("host", "rig:8791"), ("origin", "http://fox:8790"), ("access-control-request-method", "POST"), ("access-control-request-headers", "authorization, content-type")],
            &list(),
        );
        let Before::Answer(res) = r else { panic!("preflight must be answered") };
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let h = res.headers();
        assert_eq!(h["access-control-allow-origin"], "http://fox:8790");
        assert_eq!(h["access-control-allow-methods"], ALLOW_METHODS);
        assert_eq!(h["access-control-allow-headers"], ALLOW_HEADERS);
        assert_eq!(h["access-control-max-age"], "600");
        assert_eq!(h["vary"], "origin");
        assert!(!h.contains_key("access-control-allow-credentials"), "never credentials");
        assert!(!h.contains_key("access-control-allow-private-network"), "only when asked");
    }

    #[test]
    fn preflight_private_network() {
        let ask = |o: &str| {
            let (r, _) = run_before(
                Method::OPTIONS,
                &[("host", "192.168.50.3:8790"), ("origin", o), ("access-control-request-method", "GET"), ("access-control-request-private-network", "true")],
                &list(),
            );
            match r {
                Before::Answer(res) => res,
                _ => panic!(),
            }
        };
        assert_eq!(ask("https://panel.example.com").headers()["access-control-allow-private-network"], "true");
        let no = ask("https://evil.example.com");
        assert_eq!(no.status(), StatusCode::FORBIDDEN);
        assert!(!no.headers().contains_key("access-control-allow-private-network"));
        assert!(!no.headers().contains_key("access-control-allow-origin"));
    }

    #[test]
    fn preflight_for_unlisted_or_empty_list() {
        for l in [list(), vec![]] {
            let (r, _) = run_before(Method::OPTIONS, &[("host", "rig:8791"), ("origin", "http://evil:1"), ("access-control-request-method", "GET")], &l);
            let Before::Answer(res) = r else { panic!() };
            assert_eq!(res.status(), StatusCode::FORBIDDEN);
            assert!(res.headers().keys().all(|k| !k.as_str().starts_with("access-control-")), "no CORS headers at all");
        }
        // A plain OPTIONS (not a preflight) goes on to the router as before.
        let (r, _) = run_before(Method::OPTIONS, &[("host", "rig:8791")], &list());
        assert!(matches!(r, Before::Go));
    }

    #[test]
    fn answers_get_headers_only_for_listed_pages() {
        // Listed: allow-origin, expose, vary; and the cookie is dropped.
        let (r, h) = run_before(Method::GET, &[("host", "rig:8791"), ("origin", "http://fox:8790"), ("cookie", "hopperd_session=abc")], &list());
        assert!(matches!(r, Before::Go));
        assert!(!h.contains_key("cookie"), "a listed page can't ride the cookie");
        let res = after(StatusCode::OK.into_response(), Some("http://fox:8790"), true);
        assert_eq!(res.headers()["access-control-allow-origin"], "http://fox:8790");
        assert_eq!(res.headers()["access-control-expose-headers"], EXPOSE_HEADERS);
        assert_eq!(res.headers()["vary"], "origin");
        assert!(!res.headers().contains_key("access-control-allow-credentials"));

        // Unlisted: nothing but Vary; the cookie stays (same as before).
        let (r, h) = run_before(Method::GET, &[("host", "rig:8791"), ("origin", "http://evil:1"), ("cookie", "hopperd_session=abc")], &list());
        assert!(matches!(r, Before::Go));
        assert!(h.contains_key("cookie"));
        let res = after(StatusCode::OK.into_response(), None, true);
        assert!(res.headers().keys().all(|k| !k.as_str().starts_with("access-control-")));
        assert_eq!(res.headers()["vary"], "origin");

        // No Origin (desktop, scripts): untouched.
        let (r, h) = run_before(Method::GET, &[("host", "rig:8791"), ("cookie", "hopperd_session=abc")], &list());
        assert!(matches!(r, Before::Go));
        assert!(h.contains_key("cookie"));
        let res = after(StatusCode::OK.into_response(), None, false);
        assert!(!res.headers().contains_key("vary"));

        // The panel's own page, even when listed: no CORS, cookie kept.
        let (r, h) = run_before(Method::POST, &[("host", "fox:8790"), ("origin", "http://fox:8790"), ("cookie", "hopperd_session=abc")], &list());
        assert!(matches!(r, Before::Go));
        assert!(h.contains_key("cookie"));
    }

    #[test]
    fn access_management_guard() {
        let post = Method::POST;
        let other = hdrs(&[("host", "rig:8791"), ("origin", "http://fox:8790")]);
        let own = hdrs(&[("host", "rig:8791"), ("origin", "http://rig:8791")]);
        let none = hdrs(&[("host", "rig:8791")]);
        let unlisted = hdrs(&[("host", "rig:8791"), ("origin", "http://evil:1")]);
        let guarded = [
            ("/api/v1/users", Method::POST),
            ("/api/v1/users/abc", Method::PUT),
            ("/api/v1/users/abc", Method::DELETE),
            ("/api/v1/users/abc/reset-2fa", Method::POST),
            ("/api/v1/tokens", Method::POST),
            ("/api/v1/tokens/t1", Method::DELETE),
            ("/api/v1/devices/d1", Method::DELETE),
            ("/api/v1/devices/d1/manage", Method::PUT),
            ("/api/v1/pair/r1/answer", Method::POST),
            ("/api/v1/auth/2fa/begin", Method::POST),
            ("/api/v1/auth/2fa/enable", Method::POST),
            ("/api/v1/auth/2fa/disable", Method::POST),
            ("/api/v1/auth/password", Method::POST),
            ("/api/v1/settings/certificate", Method::PUT),
        ];
        for (p, m) in &guarded {
            // Switch off: refused from a listed other page.
            assert!(access_refusal(&other, &list(), false, m, p).is_some(), "{m} {p} off");
            // Switch on: allowed through (the route's own checks still run).
            assert!(access_refusal(&other, &list(), true, m, p).is_none(), "{m} {p} on");
            // The panel's own page and clients without Origin: as before.
            assert!(access_refusal(&own, &list(), false, m, p).is_none(), "{m} {p} own");
            assert!(access_refusal(&none, &list(), false, m, p).is_none(), "{m} {p} no origin");
            // Empty list: today's behaviour.
            assert!(access_refusal(&other, &[], false, m, p).is_none(), "{m} {p} empty list");
            // An unlisted page gets no CORS answer; the guard leaves it to that.
            assert!(access_refusal(&unlisted, &list(), false, m, p).is_none(), "{m} {p} unlisted");
        }
        // Reads and day-to-day work stay open from a listed page.
        assert!(access_refusal(&other, &list(), false, &Method::GET, "/api/v1/users").is_none());
        assert!(access_refusal(&other, &list(), false, &Method::GET, "/api/v1/tokens").is_none());
        for p in ["/api/v1/servers/tnp/power", "/api/v1/auth/logout", "/api/v1/auth/login", "/api/v1/userslist", "/api/v1/settings/curseforge", "/api/v1/devices-x"] {
            assert!(access_refusal(&other, &list(), false, &post, p).is_none(), "{p}");
        }
        // The list itself: only from the panel's own page, whatever the switch.
        for on in [false, true] {
            assert!(access_refusal(&other, &list(), on, &Method::PUT, SETTINGS_PATH).is_some());
            assert!(access_refusal(&unlisted, &list(), on, &Method::PUT, SETTINGS_PATH).is_some());
            assert!(access_refusal(&own, &list(), on, &Method::PUT, SETTINGS_PATH).is_none());
        }
        assert!(access_refusal(&other, &list(), false, &Method::GET, SETTINGS_PATH).is_none());
    }

    /// End to end through a router: the same two calls `security::guard` makes.
    #[tokio::test]
    async fn through_a_router() {
        use tower::ServiceExt as _;
        let l = list();
        let app = Router::new().route("/api/v1/servers", get(|| async { "ok" })).layer(axum::middleware::from_fn(move |mut req: Request, next: axum::middleware::Next| {
            let l = l.clone();
            async move {
                let had = has_origin(req.headers());
                let listed = listed_other(req.headers(), &l).map(str::to_string);
                match before(&mut req, &l) {
                    Before::Answer(r) => r,
                    Before::Go => after(next.run(req).await, listed.as_deref(), had),
                }
            }
        }));
        let call = |o: Option<&str>, m: Method| {
            let mut b = Request::builder().method(m).uri("/api/v1/servers").header("host", "rig:8791");
            if let Some(o) = o {
                b = b.header("origin", o).header("access-control-request-method", "GET");
            }
            app.clone().oneshot(b.body(B::empty()).unwrap())
        };
        let r = call(Some("http://fox:8790"), Method::GET).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()["access-control-allow-origin"], "http://fox:8790");
        let r = call(Some("http://evil:1"), Method::GET).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK, "the server answers; the browser hides it");
        assert!(!r.headers().contains_key("access-control-allow-origin"));
        let r = call(Some("http://fox:8790"), Method::OPTIONS).await.unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        let r = call(Some("http://evil:1"), Method::OPTIONS).await.unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = call(None, Method::GET).await.unwrap();
        assert!(!r.headers().contains_key("access-control-allow-origin"));
    }
}
