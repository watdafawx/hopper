//! Port clashes for a new or edited server: other servers on the same
//! machine that use the port (only one of them can run at a time), and
//! anything else listening on it right now. Desktop and Hopper Server both
//! ask this before making a server, and say which port is free instead.

use serde::{Deserialize, Serialize};

/// Where new servers' ports are looked for.
pub const RANGE: std::ops::RangeInclusive<u16> = 25565..=25700;

/// What [`check`] found for a port.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PortCheck {
    /// Other servers on the same machine set to this port.
    pub taken_by: Vec<String>,
    /// Something is listening on it now (one of those servers running, or
    /// another program).
    pub in_use: bool,
    /// A port nothing uses, when this one clashes.
    pub suggest: Option<u16>,
}

impl PortCheck {
    pub fn clashes(&self) -> bool {
        !self.taken_by.is_empty() || self.in_use
    }

    /// One sentence for players, or None when the port is fine. `host`
    /// names the machine ("this PC", "rig").
    pub fn message(&self, port: u16, host: &str) -> Option<String> {
        let who = match self.taken_by.as_slice() {
            [] => None,
            [one] => Some(format!("{one} on {host} already uses port {port}.")),
            [one, rest @ ..] => Some(format!(
                "{one} and {} other server{} on {host} already use port {port}.",
                rest.len(),
                if rest.len() == 1 { "" } else { "s" }
            )),
        };
        match (who, self.in_use) {
            (Some(w), _) => Some(format!("{w} Only one of them can run at a time.")),
            (None, true) => Some(format!("Something else on {host} is using port {port} right now.")),
            (None, false) => None,
        }
    }
}

/// Checks `port` against `used` (other servers on the machine: name, port)
/// and `free` (whether nothing listens on a port now). A clash comes with
/// the first port in [`RANGE`] that no server uses and nothing listens on.
pub fn check(port: u16, used: &[(String, u16)], free: impl Fn(u16) -> bool) -> PortCheck {
    let taken_by: Vec<String> = used.iter().filter(|(_, p)| *p == port).map(|(n, _)| n.clone()).collect();
    let in_use = !free(port);
    let suggest = (!taken_by.is_empty() || in_use)
        .then(|| next_free(used, port, &free))
        .flatten();
    PortCheck { taken_by, in_use, suggest }
}

/// The first port in [`RANGE`] (after `from` when it's in the range, then
/// from the start) that no server in `used` has and nothing listens on.
pub fn next_free(used: &[(String, u16)], from: u16, free: impl Fn(u16) -> bool) -> Option<u16> {
    let ok = |p: u16| !used.iter().any(|(_, u)| *u == p) && free(p);
    let after = if RANGE.contains(&from) { from.saturating_add(1) } else { *RANGE.start() };
    (after..=*RANGE.end()).chain(*RANGE.start()..after).find(|p| ok(*p))
}

/// [`check`] for a server on this PC: `used` from the servers Hopper
/// knows here, `free` by trying to listen on the port.
pub fn check_here(port: u16, used: &[(String, u16)]) -> PortCheck {
    check(port, used, super::port_free)
}

/// This PC's address on its network, as players nearby join it (the one
/// the default route uses; nothing is sent).
pub fn lan_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect(("192.0.2.1", 9)).ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then(|| ip.to_string())
}

/// A server's name as an address label: lower case, each run of other
/// characters one `-` (Hopper Server's New server form does the same).
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn slugs_like_the_web_form() {
        assert_eq!(super::slug("Better MC [NEOFORGE] BMC5 server"), "better-mc-neoforge-bmc5-server");
        assert_eq!(super::slug("  Ünï  "), "n");
    }

    use super::*;

    fn used() -> Vec<(String, u16)> {
        vec![("TNP-LL8".into(), 25565), ("Val".into(), 25566)]
    }

    #[test]
    fn a_port_another_server_uses_clashes_and_suggests_the_next_free() {
        let c = check(25565, &used(), |_| true);
        assert_eq!(c.taken_by, vec!["TNP-LL8".to_string()]);
        assert!(!c.in_use);
        assert_eq!(c.suggest, Some(25567));
        assert_eq!(
            c.message(25565, "rig").unwrap(),
            "TNP-LL8 on rig already uses port 25565. Only one of them can run at a time."
        );
    }

    #[test]
    fn something_listening_clashes_too() {
        let c = check(25570, &used(), |p| p != 25570 && p != 25571);
        assert!(c.taken_by.is_empty());
        assert!(c.in_use);
        assert_eq!(c.suggest, Some(25572));
        assert!(c.message(25570, "this PC").unwrap().starts_with("Something else on this PC"));
    }

    #[test]
    fn a_free_port_is_fine() {
        let c = check(25580, &used(), |_| true);
        assert!(!c.clashes());
        assert_eq!(c.suggest, None);
        assert_eq!(c.message(25580, "rig"), None);
    }

    #[test]
    fn the_search_wraps_around_the_range() {
        assert_eq!(next_free(&used(), 25700, |_| true), Some(25567));
        assert_eq!(next_free(&used(), 80, |_| true), Some(25567));
        assert_eq!(next_free(&used(), 25565, |_| false), None);
    }

    #[test]
    fn several_servers_on_one_port_are_counted() {
        let u = vec![("A".to_string(), 25565), ("B".to_string(), 25565), ("C".to_string(), 25565)];
        let m = check(25565, &u, |_| true).message(25565, "rig").unwrap();
        assert_eq!(m, "A and 2 other servers on rig already use port 25565. Only one of them can run at a time.");
    }
}
