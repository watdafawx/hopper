//! Finding the panel on the LAN: mDNS service `_hopperd._tcp.local.`, which
//! Hopper desktop (and any mDNS browser) can look up, plus the list of
//! addresses the panel answers on, for `/api/v1/hello`.

use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::net::IpAddr;

pub const SERVICE: &str = "_hopperd._tcp.local.";

/// Virtual adapters (Hyper-V, WSL, Docker, VPN overlays): reachable from
/// this machine only, or not the home network.
fn is_virtual(adapter: &str) -> bool {
    let a = adapter.to_lowercase();
    ["vethernet", "wsl", "docker", "hyper-v", "virtualbox", "vmware", "zerotier", "hamachi", "radmin"]
        .iter()
        .any(|v| a.contains(v))
}

/// How likely an address is the one other machines on the LAN use; lower
/// is better.
fn rank(ip: &std::net::Ipv4Addr, adapter: &str) -> u8 {
    let o = ip.octets();
    let base = match o {
        [192, 168, ..] => 0,
        [10, ..] => 1,
        [172, 16..=31, ..] => 2,
        // Tailscale's range: reachable, but not the LAN.
        [100, 64..=127, ..] => 3,
        _ => 4,
    };
    if is_virtual(adapter) { base + 10 } else { base }
}

/// This machine's addresses (IPv4, not loopback or link-local), the most
/// likely LAN address first.
pub fn lan_addresses() -> Vec<IpAddr> {
    let mut out: Vec<(u8, IpAddr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            IpAddr::V4(v4) if !v4.is_link_local() => Some((rank(&v4, &i.name), IpAddr::V4(v4))),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup_by_key(|x| x.1);
    out.into_iter().map(|x| x.1).collect()
}

/// Announces the panel; keep the daemon alive as long as it should be found.
pub fn announce(name: &str, port: u16, addrs: &[IpAddr]) -> anyhow::Result<ServiceDaemon> {
    let mdns = ServiceDaemon::new()?;
    let host = format!("{}.local.", crate::servers::slug(name));
    let props = [("version", env!("CARGO_PKG_VERSION")), ("api", "1"), ("path", "/api/v1")];
    // Auto addresses: follows the network card as it gets (or changes) its
    // address after hopperd started.
    let info = ServiceInfo::new(SERVICE, name, &host, addrs, port, &props[..])?.enable_addr_auto();
    mdns.register(info)?;
    Ok(mdns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "network: multicast on the LAN"]
    fn announced_panel_can_be_browsed() {
        let name = format!("hopperd-test-{}", std::process::id());
        let addrs = lan_addresses();
        let _d = announce(&name, 18790, &addrs).unwrap();
        let browser = ServiceDaemon::new().unwrap();
        let rx = browser.browse(SERVICE).unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < end {
            if let Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) = rx.recv_timeout(std::time::Duration::from_millis(500))
                && info.get_fullname().starts_with(&name)
            {
                assert_eq!(info.get_port(), 18790);
                assert_eq!(info.get_property_val_str("api"), Some("1"));
                return;
            }
        }
        panic!("{name} wasn't found on the LAN");
    }

    #[test]
    fn home_lan_beats_virtual_and_vpn() {
        let r = |ip: &str, name| rank(&ip.parse().unwrap(), name);
        assert!(r("192.168.50.2", "Ethernet 3") < r("10.66.0.2", "voice"));
        assert!(r("10.66.0.2", "voice") < r("172.24.0.1", "vEthernet (Default Switch)"));
        assert!(r("100.101.1.2", "Tailscale") < r("10.147.18.28", "ZeroTier One [1c33]"));
    }
}
