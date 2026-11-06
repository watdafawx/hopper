//! Links to Hopper's GitHub page and its "Report an issue" form. The form
//! (`.github/ISSUE_TEMPLATE/bug_report.yml`) reads its fields from the
//! link's query, so the app, version and OS arrive filled in. Nothing else
//! is sent: logs stay on the player's PC unless they share one themselves.

/// The project page, for the GitHub button.
pub const REPO_URL: &str = "https://github.com/watdafawx/hopper";

/// Which app is reporting. The names match the form's App dropdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum App {
    Hopper,
    Server,
}

impl App {
    fn name(self) -> &'static str {
        match self {
            App::Hopper => "Hopper",
            App::Server => "Hopper Server",
        }
    }
}

/// The OS as players would write it, e.g. "Windows (x86_64)".
pub fn os_label() -> String {
    let os = match std::env::consts::OS {
        "windows" => "Windows",
        "linux" => "Linux",
        "macos" => "macOS",
        other => other,
    };
    format!("{os} ({})", std::env::consts::ARCH)
}

/// The bug form for `app` at `version`, with `title` (e.g. "Crash: …")
/// filled in when given.
pub fn issue_url(app: App, version: &str, title: Option<&str>) -> String {
    let mut url = format!(
        "{REPO_URL}/issues/new?template=bug_report.yml&app={}&version={}&os={}",
        encode(app.name()),
        encode(version),
        encode(&os_label())
    );
    if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
        // A crash headline can be long; GitHub titles read best short.
        let t: String = t.chars().take(120).collect();
        url.push_str("&title=");
        url.push_str(&encode(&t));
    }
    url
}

/// Percent-encodes a query value (everything but RFC 3986 unreserved).
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_app_version_and_os() {
        let u = issue_url(App::Server, "0.43.0", None);
        assert!(u.starts_with("https://github.com/watdafawx/hopper/issues/new?template=bug_report.yml&app=Hopper%20Server&version=0.43.0&os="));
        assert!(!u.contains("title="));
    }

    #[test]
    fn encodes_and_trims_title() {
        let u = issue_url(App::Hopper, "0.47.0", Some("  Crash: Mixin apply failed & more?  "));
        assert!(u.ends_with("&title=Crash%3A%20Mixin%20apply%20failed%20%26%20more%3F"), "{u}");
        let blank = issue_url(App::Hopper, "1", Some("  "));
        assert!(!blank.contains("title="));
        let long = issue_url(App::Hopper, "1", Some(&"é".repeat(300)));
        assert_eq!(long.matches("%C3%A9").count(), 120);
    }
}
