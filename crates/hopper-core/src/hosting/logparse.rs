//! Reading a server's console lines: when it is up, who joined or left,
//! what `list` says, and the failures worth explaining.

/// Something a console line says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogEvent {
    /// `Done (12.3s)! For help, type "help"`: the server is up.
    Done,
    Joined(String),
    Left(String),
    /// The reply to `list`: (online, max, names).
    List(u32, u32, Vec<String>),
    /// `Starting minecraft server version 1.21.1`.
    Version(String),
    /// `Stopping server`.
    Stopping,
    /// `**** FAILED TO BIND TO PORT!`: another program has the port.
    PortInUse,
    /// The EULA in eula.txt isn't accepted.
    EulaNotAccepted,
}

/// The message after the `[time] [thread/LEVEL] [logger]:` prefix.
pub fn message(line: &str) -> &str {
    let line = line.trim_end();
    // Vanilla: "[12:00:00] [Server thread/INFO]: msg"; Forge/NeoForge:
    // "[12:00:00] [Server thread/INFO] [minecraft/MinecraftServer]: msg".
    if line.starts_with('[')
        && let Some(i) = line.find("]: ")
    {
        return &line[i + 3..];
    }
    line
}

pub fn is_player_name(s: &str) -> bool {
    (1..=16).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Strips Minecraft's `§x` formatting codes.
fn strip_codes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// "Steve" from "Steve joined the game" or "Steve (formerly known as
/// Alex) joined the game"; `None` for chat that only looks like it.
fn subject(msg: &str, suffix: &str) -> Option<String> {
    let who = msg.strip_suffix(suffix)?;
    let who = who.split(" (formerly known as ").next().unwrap_or(who);
    is_player_name(who).then(|| who.to_string())
}

pub fn parse_line(line: &str) -> Option<LogEvent> {
    let raw = strip_codes(line);
    let msg = message(&raw).trim();
    if msg.starts_with("Done (") && msg.contains(")! For help") {
        return Some(LogEvent::Done);
    }
    if let Some(n) = subject(msg, " joined the game") {
        return Some(LogEvent::Joined(n));
    }
    if let Some(n) = subject(msg, " left the game") {
        return Some(LogEvent::Left(n));
    }
    if let Some(v) = msg.strip_prefix("Starting minecraft server version ") {
        return Some(LogEvent::Version(v.trim().to_string()));
    }
    if msg == "Stopping server" || msg == "Stopping the server" {
        return Some(LogEvent::Stopping);
    }
    if msg.contains("FAILED TO BIND TO PORT") {
        return Some(LogEvent::PortInUse);
    }
    if msg.contains("You need to agree to the EULA") {
        return Some(LogEvent::EulaNotAccepted);
    }
    parse_list(msg)
}

/// "There are 2 of a max of 20 players online: Alex, Steve" (1.13+) or
/// "There are 2/20 players online:" (older; names come on the next line,
/// which this doesn't read).
fn parse_list(msg: &str) -> Option<LogEvent> {
    let rest = msg.strip_prefix("There are ")?;
    let (counts, names) = rest.split_once(" players online:")?;
    let (online, max) = if let Some((a, b)) = counts.split_once(" of a max of ") {
        (a.trim().parse().ok()?, b.trim().parse().ok()?)
    } else {
        let (a, b) = counts.split_once('/')?;
        (a.trim().parse().ok()?, b.trim().parse().ok()?)
    };
    let names = names
        .split(',')
        .map(str::trim)
        .filter(|n| is_player_name(n))
        .map(String::from)
        .collect();
    Some(LogEvent::List(online, max, names))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_and_players() {
        assert_eq!(
            parse_line("[10:00:01] [Server thread/INFO]: Starting minecraft server version 1.21.1"),
            Some(LogEvent::Version("1.21.1".into()))
        );
        assert_eq!(
            parse_line("[10:00:31] [Server thread/INFO] [minecraft/DedicatedServer]: Done (28.512s)! For help, type \"help\""),
            Some(LogEvent::Done)
        );
        assert_eq!(
            parse_line("[10:01:00] [Server thread/INFO] [minecraft/MinecraftServer]: Steve_2 joined the game"),
            Some(LogEvent::Joined("Steve_2".into()))
        );
        assert_eq!(
            parse_line("[10:01:00] [Server thread/INFO]: §eAlex (formerly known as Al) joined the game"),
            Some(LogEvent::Joined("Alex".into()))
        );
        assert_eq!(
            parse_line("[10:02:00] [Server thread/INFO]: Steve_2 left the game"),
            Some(LogEvent::Left("Steve_2".into()))
        );
        // Chat that mimics a join is not one.
        assert_eq!(parse_line("[10:02:00] [Server thread/INFO]: <Bob> Eve joined the game"), None);
        assert_eq!(
            parse_line("[10:03:00] [Server thread/INFO]: There are 2 of a max of 20 players online: Alex, Steve"),
            Some(LogEvent::List(2, 20, vec!["Alex".into(), "Steve".into()]))
        );
        assert_eq!(
            parse_line("[10:03:00] [Server thread/INFO]: There are 0 of a max of 20 players online: "),
            Some(LogEvent::List(0, 20, vec![]))
        );
        assert_eq!(
            parse_line("[10:03:00] [Server thread/INFO]: There are 1/10 players online:"),
            Some(LogEvent::List(1, 10, vec![]))
        );
    }

    #[test]
    fn failures() {
        assert_eq!(
            parse_line("[10:00:02] [Server thread/WARN]: **** FAILED TO BIND TO PORT!"),
            Some(LogEvent::PortInUse)
        );
        assert_eq!(
            parse_line("[10:00:02] [ServerMain/INFO]: You need to agree to the EULA in order to run the server. Go to eula.txt for more info."),
            Some(LogEvent::EulaNotAccepted)
        );
        assert_eq!(parse_line("[10:00:02] [Server thread/INFO]: Stopping server"), Some(LogEvent::Stopping));
        assert_eq!(parse_line("random output"), None);
    }
}
