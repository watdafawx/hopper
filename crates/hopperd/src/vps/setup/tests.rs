use super::plan::*;

#[test]
fn hosts_and_users_are_checked_before_any_command() {
    assert_eq!(valid_host(" 203.0.113.10 ").unwrap(), "203.0.113.10");
    assert_eq!(valid_host("VPS.Example.com").unwrap(), "vps.example.com");
    assert_eq!(valid_host("[2001:db8::1]").unwrap(), "2001:db8::1");
    assert_eq!(valid_host("2001:db8::1").unwrap(), "2001:db8::1");
    for bad in ["", "a b", "-oProxyCommand=x", "host;rm -rf /", "$(id)", "a..b", "host`x`", "a/b", "x'y", "name-.com", ".x"] {
        assert!(valid_host(bad).is_err(), "{bad:?}");
    }
    assert_eq!(valid_user("ubuntu").unwrap(), "ubuntu");
    assert!(valid_user("opc").is_ok() && valid_user("root").is_ok() && valid_user("_svc-1.x").is_ok());
    for bad in ["", "-x", "Root", "a b", "a;b", "$USER", "a'b", "averyveryveryveryverylongusername1"] {
        assert!(valid_user(bad).is_err(), "{bad:?}");
    }
    assert!(valid_port(80).is_err() && valid_port(25565).is_ok());
}

#[test]
fn quoting_keeps_a_string_one_word() {
    assert_eq!(sh_quote("abc"), "'abc'");
    assert_eq!(sh_quote("it's"), r"'it'\''s'");
    // A command run as root is one quoted argument to sh, whatever it holds.
    let c = as_root(false, "echo 'a b' && id");
    assert_eq!(c, r"sudo -n sh -c 'echo '\''a b'\'' && id'");
    assert!(as_root(true, "id").starts_with("sh -c "));
    assert_eq!(as_relay("x"), "runuser -u hopper-relay -- x");
}

#[test]
fn os_release_arch_memory_disk_and_ports_are_read() {
    let u = parse_os_release("PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n");
    assert_eq!((u.id.as_str(), u.version_id.as_str(), u.pretty.as_str()), ("ubuntu", "24.04", "Ubuntu 24.04.1 LTS"));
    assert_eq!(os_supported(&u).unwrap(), "Ubuntu 24.04");
    let d = parse_os_release("PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nVERSION_ID=\"12\"\nID=debian\n");
    assert_eq!(os_supported(&d).unwrap(), "Debian 12");
    assert!(os_supported(&parse_os_release("ID=debian\nVERSION_ID=\"13\"")).is_ok());
    assert!(os_supported(&parse_os_release("ID=ubuntu\nVERSION_ID=\"22.04\"")).is_ok());
    let rocky = parse_os_release("NAME=\"Rocky Linux\"\nVERSION_ID=\"9.4\"\nID=\"rocky\"\nPRETTY_NAME=\"Rocky Linux 9.4 (Blue Onyx)\"");
    assert_eq!(os_supported(&rocky).unwrap_err(), "Rocky Linux 9.4 (Blue Onyx) isn't supported yet. The relay runs on Ubuntu 22.04 or 24.04 and Debian 12 or 13.");
    assert!(os_supported(&parse_os_release("ID=ubuntu\nVERSION_ID=\"20.04\"")).is_err());

    assert_eq!(parse_arch("x86_64\n"), Some("x64"));
    assert_eq!(parse_arch("aarch64"), Some("arm64"));
    assert_eq!(parse_arch("armv7l"), None);

    assert_eq!(parse_meminfo("MemTotal:       16314880 kB\nMemFree:  1 kB\n"), Some(16314880 * 1024));
    assert_eq!(parse_meminfo("nothing"), None);

    let df = "Filesystem     1024-blocks    Used Available Capacity Mounted on\n/dev/sda1       100000000 1000000  92274688       2% /\n/dev/sdb1       100000000 1000000   1048576      50% /var/lib\n";
    assert_eq!(parse_df_avail(df), Some(1048576 * 1024));

    let ss = "LISTEN 0      4096   127.0.0.53%lo:53        0.0.0.0:*\nLISTEN 0      128          0.0.0.0:22        0.0.0.0:*\nLISTEN 0      128             [::]:22           [::]:*\nLISTEN 0 1024 127.0.0.1:25600 0.0.0.0:*\n";
    assert_eq!(parse_listening(ss), vec![22, 53, 25600]);
}

#[test]
fn memory_picks_the_cache_size() {
    use hopper_core::hosting::relay_codec::CodecConfig;
    let small = CodecConfig::for_memory(2 << 30).cache_bytes;
    let big = CodecConfig::for_memory(12 << 30).cache_bytes;
    assert_eq!(small, 512 << 20);
    assert_eq!(big, 1 << 30);
    assert_eq!(size(12 << 30), "12 GB");
    assert_eq!(size((19 << 30) / 10), "1.9 GB");
    assert_eq!(size(512 << 20), "512 MB");
}

#[test]
fn relay_status_and_elf_headers_are_read() {
    let out = "some line\n{\"version\":\"0.72.7\",\"fingerprint\":\"AB:CD\",\"key_sha256\":\"00\",\"bind\":\"0.0.0.0\",\"game_port\":25565,\"tunnel_port\":25566}\n";
    let s: RelayStatus = last_json(out).unwrap();
    assert_eq!((s.fingerprint.as_str(), s.game_port, s.tunnel_port), ("AB:CD", 25565, 25566));
    let mut h = vec![0u8; 20];
    h[..4].copy_from_slice(b"\x7fELF");
    h[18] = 0x3e;
    assert_eq!(elf_arch(&h), Some("x64"));
    h[18] = 0xb7;
    assert_eq!(elf_arch(&h), Some("arm64"));
    assert_eq!(elf_arch(b"MZ\x90\x00"), None);
}

/// Whatever the firewall looks like, the plan only adds ACCEPTs for the
/// relay's two ports: never a policy, a DROP/REJECT, an enable, or the ssh
/// port, so it can't cut the connection it runs over.
fn safe(p: &FwPlan) {
    for c in &p.add {
        let l = c.to_lowercase();
        for bad in [" -p input", "-p input", " drop", " reject", "enable", "default", "deny", "reset", "--dport 22 ", "allow 22/", " -d input", "flush", " -f"] {
            assert!(!l.contains(bad), "{c:?} has {bad:?}");
        }
        assert!(c.starts_with("ufw allow ") || c.starts_with("iptables -I INPUT ") || c == "netfilter-persistent save", "{c:?}");
    }
}

const PORTS: [u16; 2] = [25565, 25566];

#[test]
fn ufw_on_gets_an_allow_for_each_port_it_lacks() {
    let st = "Status: active\n\nTo                         Action      From\n--                         ------      ----\n22/tcp                     ALLOW       Anywhere\n25566/tcp                  ALLOW       Anywhere\n22/tcp (v6)                ALLOW       Anywhere (v6)\n";
    let p = plan_firewall(Some(st), Some("-P INPUT DROP\n-A INPUT -j ufw-before-input\n"), None, true, &PORTS);
    safe(&p);
    assert_eq!(p.add, vec!["ufw allow 25565/tcp comment hopper-relay"]);
    assert_eq!(p.rules, vec![FwRule::Ufw { port: 25565 }]);
    assert_eq!(p.open, vec![25566]);
    // ufw off: its rules don't count, iptables is read instead.
    let p = plan_firewall(Some("Status: inactive\n"), Some("-P INPUT ACCEPT\n"), None, false, &PORTS);
    assert!(p.add.is_empty() && p.open == PORTS && p.manual.is_none());
}

#[test]
fn iptables_accepts_go_just_before_the_images_reject() {
    // A cloud image's INPUT: established, icmp, lo, ssh, then a catch-all REJECT.
    let rules = "-P INPUT ACCEPT\n-A INPUT -m state --state RELATED,ESTABLISHED -j ACCEPT\n-A INPUT -p icmp -j ACCEPT\n-A INPUT -i lo -j ACCEPT\n-A INPUT -p tcp -m state --state NEW -m tcp --dport 22 -j ACCEPT\n-A INPUT -j REJECT --reject-with icmp-host-prohibited\n";
    assert_eq!(iptables_block_at(rules, &PORTS), Some(5));
    let p = plan_firewall(None, Some(rules), None, true, &PORTS);
    safe(&p);
    assert_eq!(
        p.add,
        vec![
            "iptables -I INPUT 5 -p tcp --dport 25565 -m comment --comment hopper-relay -j ACCEPT",
            "iptables -I INPUT 6 -p tcp --dport 25566 -m comment --comment hopper-relay -j ACCEPT",
            "netfilter-persistent save",
        ]
    );
    assert!(p.saved);
    // Without netfilter-persistent nothing claims they were saved.
    let p = plan_firewall(None, Some(rules), None, false, &PORTS);
    assert!(!p.saved && p.add.len() == 2);
    // A REJECT for these ports only also counts; one for other ports doesn't.
    let r2 = "-P INPUT ACCEPT\n-A INPUT -p tcp -m tcp --dport 8080 -j DROP\n-A INPUT -p tcp -m multiport --dports 25565,25566 -j REJECT --reject-with tcp-reset\n";
    assert_eq!(iptables_block_at(r2, &PORTS), Some(2));
    assert_eq!(plan_firewall(None, Some(r2), None, false, &PORTS).add[0], "iptables -I INPUT 2 -p tcp --dport 25565 -m comment --comment hopper-relay -j ACCEPT");
    // Interface- or sender-only rejects don't turn players away.
    assert_eq!(iptables_block_at("-P INPUT ACCEPT\n-A INPUT -i docker0 -j DROP\n-A INPUT -s 10.0.0.0/8 -j REJECT\n", &PORTS), None);
}

#[test]
fn iptables_default_drop_appends_and_open_ports_are_left_alone() {
    let rules = "-P INPUT DROP\n-A INPUT -i lo -j ACCEPT\n-A INPUT -p tcp --dport 22 -j ACCEPT\n-A INPUT -p tcp --dport 25565 -j ACCEPT\n";
    let p = plan_firewall(None, Some(rules), None, false, &PORTS);
    safe(&p);
    assert_eq!(p.open, vec![25565]);
    assert_eq!(p.add, vec!["iptables -I INPUT 4 -p tcp --dport 25566 -m comment --comment hopper-relay -j ACCEPT"]);
    // Re-running after the setup added them: nothing to do (repair = no change).
    let after = "-P INPUT ACCEPT\n-A INPUT -i lo -j ACCEPT\n-A INPUT -p tcp -m tcp --dport 25565 -m comment --comment hopper-relay -j ACCEPT\n-A INPUT -p tcp -m tcp --dport 25566 -m comment --comment hopper-relay -j ACCEPT\n-A INPUT -j REJECT --reject-with icmp-host-prohibited\n";
    let p = plan_firewall(None, Some(after), None, true, &PORTS);
    assert!(p.add.is_empty() && p.rules.is_empty(), "{p:?}");
    assert_eq!(p.open, PORTS.to_vec());
    assert_eq!(iptables_marked(after), vec![25565, 25566]);
    // Nothing filtering at all.
    let p = plan_firewall(None, Some("-P INPUT ACCEPT\n"), None, true, &PORTS);
    assert!(p.add.is_empty() && p.open == PORTS);
}

#[test]
fn native_nftables_that_drops_is_left_to_the_player() {
    let nft = "table inet filter {\n\tchain input {\n\t\ttype filter hook input priority filter; policy drop;\n\t\ttcp dport 22 accept\n\t}\n}\n";
    assert!(nft_blocks_input(nft));
    let p = plan_firewall(None, None, Some(nft), false, &PORTS);
    assert!(p.add.is_empty());
    assert!(p.manual.as_deref().unwrap().contains("25565/tcp and 25566/tcp"));
    // iptables-nft's own table isn't native: read through iptables.
    assert!(!nft_blocks_input("table ip filter {\n\tchain INPUT {\n\t\ttype filter hook input priority filter; policy drop;\n\t}\n}\n"));
    assert!(!nft_blocks_input("table inet filter {\n\tchain input {\n\t\ttype filter hook input priority filter; policy accept;\n\t}\n}\n"));
}

#[test]
fn removing_takes_out_exactly_what_was_added() {
    let rules = vec![FwRule::Ufw { port: 25565 }, FwRule::Iptables { port: 25566 }];
    let v = remove_firewall(&rules, true);
    assert_eq!(v[0], "ufw delete allow 25565/tcp");
    assert_eq!(v[1], "while iptables -C INPUT -p tcp --dport 25566 -m comment --comment hopper-relay -j ACCEPT 2>/dev/null; do iptables -D INPUT -p tcp --dport 25566 -m comment --comment hopper-relay -j ACCEPT; done");
    assert_eq!(v[2], "netfilter-persistent save");
    assert!(remove_firewall(&[FwRule::Ufw { port: 1 }], true).len() == 1, "ufw keeps its own rules");
    assert!(remove_firewall(&[], true).is_empty());
    // The record round-trips.
    let i = Installed { made_user: true, firewall: rules, binary_sha256: "ab".into(), binary_version: "1".into(), ..Default::default() };
    let back: Installed = serde_json::from_str(&serde_json::to_string(&i).unwrap()).unwrap();
    assert_eq!(back, i);
    assert!(serde_json::to_string(&i).unwrap().contains(r#"{"kind":"ufw","port":25565}"#));
}

#[test]
fn the_unit_runs_unprivileged_and_hardened() {
    let u = unit();
    for want in ["User=hopper-relay", "Group=hopper-relay", "ExecStart=/opt/hopper-relay/hopperd relay run --data /var/lib/hopper-relay", "NoNewPrivileges=true", "ProtectSystem=strict", "ProtectHome=true", "PrivateTmp=true", "ReadWritePaths=/var/lib/hopper-relay", "CapabilityBoundingSet=\n", "WantedBy=multi-user.target"] {
        assert!(u.contains(want), "{want}");
    }
    assert!(!u.contains("root"));
}

#[test]
fn each_step_sees_when_it_has_nothing_to_do() {
    // Folders.
    let good = "/opt/hopper-relay root root 755\n/var/lib/hopper-relay hopper-relay hopper-relay 700\n";
    assert!(folders_right(true, good));
    assert!(!folders_right(false, good));
    assert!(!folders_right(true, "/opt/hopper-relay root root 777\n/var/lib/hopper-relay hopper-relay hopper-relay 700\n"));
    // The relay's files.
    assert!(data_private("hopper-relay 700\nhopper-relay 600\nhopper-relay 600\n"));
    assert!(!data_private("hopper-relay 700\nhopper-relay 644\n"));
    assert!(!data_private("root 700\nhopper-relay 600\n"));
    // The program.
    assert!(binary_right("aa", "aa", "hopperd 0.72.7\n", "0.72.7", None));
    assert!(!binary_right("aa", "aa", "hopperd 0.72.6", "0.72.7", None), "an older relay is updated");
    assert!(!binary_right("aa", "bb", "hopperd 0.72.7", "0.72.7", None), "a changed file is put back");
    assert!(!binary_right("", "", "", "0.72.7", None));
    assert!(binary_right("ff", "", "", "0.72.7", Some("ff")));
    assert!(!binary_right("aa", "aa", "hopperd 0.72.7", "0.72.7", Some("ff")));
    // The key.
    let key = "hrt_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    let hash = crate::vps::link::hash_hex(&crate::vps::link::key_hash(key));
    let st = RelayStatus { key_sha256: hash, fingerprint: "FP".into(), bind: "0.0.0.0".into(), game_port: 25565, tunnel_port: 25566, velocity_port: VELOCITY_PORT, backend_port: BACKEND_PORT, ..Default::default() };
    let b2 = RelayStatus { velocity_port: 0, backend_port: 0, ..st.clone() };
    assert_eq!(keys_right(Some(&b2), key, "FP", "h:25566", "h:25566", "0.0.0.0", (25565, 25566), true), (true, false), "a B2 relay gets Velocity's ports, same key");
    let ports = (25565, 25566);
    assert_eq!(keys_right(Some(&st), key, "FP", "h:25566", "h:25566", "0.0.0.0", ports, true), (true, true));
    assert_eq!(keys_right(Some(&st), key, "FP", "h:25566", "h:25566", "127.0.0.1", ports, true), (true, false), "new settings keep the key");
    assert_eq!(keys_right(Some(&st), key, "FP", "h:25566", "h:25566", "0.0.0.0", (25570, 25566), true), (true, false));
    assert_eq!(keys_right(Some(&st), key, "FP", "h:25566", "h:25566", "0.0.0.0", ports, false), (true, false), "loose files are locked again");
    assert_eq!(keys_right(Some(&st), "", "FP", "h:25566", "h:25566", "0.0.0.0", ports, true), (false, false), "home lost its key: a new one");
    assert_eq!(keys_right(Some(&st), key, "OTHER", "h:25566", "h:25566", "0.0.0.0", ports, true), (false, false));
    assert_eq!(keys_right(None, key, "FP", "h:25566", "h:25566", "0.0.0.0", ports, true), (false, false));
    // The service.
    assert!(service_acts(&unit(), "enabled\n", "active\n", false).is_empty());
    assert_eq!(service_acts(&unit(), "enabled", "active", true), vec![ServiceAct::Restart]);
    assert_eq!(service_acts("", "not-found", "inactive", false), vec![ServiceAct::WriteUnit, ServiceAct::Enable, ServiceAct::Restart]);
    assert_eq!(service_acts(&unit(), "disabled", "active", false), vec![ServiceAct::Enable]);
    assert_eq!(service_acts("old", "enabled", "active", false), vec![ServiceAct::WriteUnit, ServiceAct::Restart]);
}

#[test]
fn a_new_key_reads_back_and_a_passphrase_comes_off() {
    let (private, public) = super::ssh::new_key().unwrap();
    assert!(private.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
    assert!(public.starts_with("ssh-ed25519 ") && public.ends_with("hopper-relay-setup"));
    let (again, pub2) = super::ssh::read_key(&private, "").unwrap();
    assert_eq!((again.as_str(), pub2.as_str()), (private.as_str(), public.as_str()));
    assert!(super::ssh::read_key("ssh-ed25519 AAAA nope", "").is_err());
    // Logged by mistake, it still wouldn't show.
    assert!(!hopper_core::applog::redact(&format!("key {private}")).contains(&private[40..60]));
}

#[test]
fn install_and_remove_list_their_steps() {
    let ids: Vec<&str> = install_steps("0.72.7", (25565, 25566)).iter().map(|s| s.id).collect();
    assert_eq!(ids, ["user", "binary", "java", "velocity", "keys", "service", "velocity-service", "firewall", "reach", "pair"]);
    assert!(install_steps("0.72.7", (25565, 25566)).iter().all(|s| s.state == "wait"));
    let ids: Vec<&str> = remove_steps().iter().map(|s| s.id).collect();
    assert_eq!(ids, ["service", "firewall", "files", "user", "pair", "login"]);
}

#[test]
fn velocity_is_pinned_and_set_up_for_the_chain() {
    // Pinned downloads: 64 hex each, https only.
    for p in [VELOCITY, java("x64").unwrap(), java("arm64").unwrap()] {
        assert!(p.sha256.len() == 64 && p.sha256.chars().all(|c| c.is_ascii_hexdigit()), "{}", p.name);
        assert!(p.url.starts_with("https://") && !p.url.contains(['\'', ' ']));
        assert!(p.size > 1_000_000);
    }
    assert!(java("riscv").is_none());
    let t = velocity_toml(true);
    assert!(t.contains("player-info-forwarding-mode = \"modern\""));
    assert!(t.contains(&format!("bind = \"127.0.0.1:{VELOCITY_PORT}\"")));
    assert!(t.contains(&format!("home = \"127.0.0.1:{BACKEND_PORT}\"")));
    assert!(t.contains("online-mode = true") && t.contains("haproxy-protocol = true"));
    assert!(t.contains(&format!("forwarding-secret-file = \"{SECRET}\"")));
    assert!(velocity_toml(false).contains("online-mode = false"));
    let u = velocity_unit();
    for flag in ["-Dvelocity.max-plugin-message-payload-size.clientbound=16777216", "-Dvelocity.max-plugin-message-payload-size.serverbound=16777216", "-Dvelocity.max-known-packs=1024"] {
        assert!(u.contains(flag), "{flag}");
    }
    assert!(u.contains(&format!("User={VELOCITY_USER}")) && u.contains(&format!("ReadWritePaths={VELOCITY_DATA}")));
    assert!(!u.contains("MemoryDenyWriteExecute"), "Java's JIT needs it");
    let stat = format!("{ETC} root {VELOCITY_USER} 750\n{SECRET} root {VELOCITY_USER} 640\n{VELOCITY_TOML} root {VELOCITY_USER} 640\n{VELOCITY_DATA} {VELOCITY_USER} {VELOCITY_USER} 700\n{VELOCITY_JAR} root root 644\n");
    assert!(velocity_files_right(&stat));
    assert!(!velocity_files_right(&stat.replace("640", "644")), "the secret readable by all is locked again");
}
