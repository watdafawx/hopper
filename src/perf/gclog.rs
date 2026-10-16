//! Reads the GC log Java writes with `-Xlog:gc*` (unified logging, Java 9+):
//! every pause, the heap in use before each collection, the collector and
//! the Java version.

use super::stats;
use serde::{Deserialize, Serialize};

/// What one GC log says about a run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GcStats {
    /// "G1", "ZGC", "Shenandoah", "Parallel", "Serial", or empty.
    pub collector: String,
    /// "21.0.5+9-LTS", from the log's init lines.
    pub java_version: String,
    pub pauses: u32,
    pub total_pause_ms: f64,
    pub avg_pause_ms: f64,
    pub p99_pause_ms: f64,
    pub max_pause_ms: f64,
    /// Most heap in use, in MB (the largest "before" of any collection).
    pub peak_heap_mb: f64,
    /// The heap's maximum size, in MB, when the log says.
    pub max_heap_mb: f64,
}

/// One log line split into its uptime (seconds), tags and message.
/// `[1.234s][info][gc,start    ] GC(0) Pause Young` -> (1.234, "gc,start", "GC(0) Pause Young").
fn split_line(line: &str) -> Option<(Option<f64>, &str, &str)> {
    let mut rest = line.trim_end();
    let mut groups = Vec::new();
    while let Some(r) = rest.strip_prefix('[') {
        let end = r.find(']')?;
        groups.push(r[..end].trim());
        rest = &r[end + 1..];
    }
    let tags = *groups.last()?;
    let uptime = groups
        .iter()
        .find_map(|g| g.strip_suffix('s').and_then(|n| n.parse::<f64>().ok()));
    Some((uptime, tags, rest.trim()))
}

/// A duration at the end of a message: "… 5.123ms" or "… 0.080s", in ms.
fn trailing_ms(msg: &str) -> Option<f64> {
    let last = msg.rsplit(' ').next()?;
    if let Some(n) = last.strip_suffix("ms") {
        return n.parse().ok();
    }
    if let Some(n) = last.strip_suffix("us") {
        return n.parse::<f64>().ok().map(|v| v / 1000.0);
    }
    last.strip_suffix('s')?.parse::<f64>().ok().map(|v| v * 1000.0)
}

/// "24M" / "1024K" / "2G" in MB.
fn size_mb(num: &str, unit: char) -> Option<f64> {
    let n: f64 = num.parse().ok()?;
    Some(match unit {
        'K' => n / 1024.0,
        'M' => n,
        'G' => n * 1024.0,
        'B' => n / (1024.0 * 1024.0),
        _ => return None,
    })
}

/// The heap before a collection: the first `<n><unit>` (with ZGC's
/// optional `(12%)`) right before a `->`.
fn heap_before_mb(msg: &str) -> Option<f64> {
    let arrow = msg.find("->")?;
    let left = &msg[..arrow];
    // Drop ZGC's "(1%)".
    let left = match left.strip_suffix(')') {
        Some(l) => &l[..l.rfind('(')?],
        None => left,
    };
    let token = left.rsplit(' ').next()?;
    let unit = token.chars().last()?;
    size_mb(&token[..token.len() - unit.len_utf8()], unit)
}

fn collector(msg: &str) -> Option<&'static str> {
    let m = msg.strip_prefix("Using ")?;
    Some(if m.starts_with("G1") {
        "G1"
    } else if m.starts_with("The Z Garbage Collector") || m.starts_with("ZGC") {
        "ZGC"
    } else if m.starts_with("Shenandoah") {
        "Shenandoah"
    } else if m.starts_with("Parallel") {
        "Parallel"
    } else if m.starts_with("Serial") {
        "Serial"
    } else if m.starts_with("Epsilon") {
        "Epsilon"
    } else {
        return None;
    })
}

/// Parses a whole `-Xlog:gc*` log.
pub fn parse(text: &str) -> GcStats {
    let mut s = GcStats::default();
    let mut pauses: Vec<f64> = Vec::new();
    for line in text.lines() {
        let Some((_, tags, msg)) = split_line(line) else { continue };
        let first_tag = tags.split(',').next().unwrap_or("");
        if first_tag != "gc" {
            continue;
        }
        if let Some(c) = collector(msg) {
            s.collector = c.into();
            continue;
        }
        if let Some(v) = msg.strip_prefix("Version: ") {
            s.java_version = v.split(' ').next().unwrap_or(v).to_string();
            continue;
        }
        if let Some(v) = msg.strip_prefix("Heap Max Capacity: ") {
            let v = v.trim();
            if let Some(u) = v.chars().last()
                && let Some(mb) = size_mb(&v[..v.len() - u.len_utf8()], u)
            {
                s.max_heap_mb = mb;
            }
            continue;
        }
        // Pauses: "GC(3) Pause Young (Normal) (G1 Evacuation Pause) 24M->8M(256M) 5.1ms",
        // ZGC's "GC(3) Y: Pause Mark Start 0.010ms", Shenandoah's "GC(0) Pause Init Mark 0.1ms".
        if msg.contains("Pause ")
            && let Some(ms) = trailing_ms(msg)
        {
            pauses.push(ms);
        }
        // Heap sizes only from the plain `gc` summary lines; `gc,metaspace`
        // and the like have their own "->" numbers.
        if tags == "gc"
            && let Some(mb) = heap_before_mb(msg)
        {
            s.peak_heap_mb = s.peak_heap_mb.max(mb);
        }
    }
    s.pauses = pauses.len() as u32;
    s.total_pause_ms = pauses.iter().sum();
    s.avg_pause_ms = if pauses.is_empty() { 0.0 } else { s.total_pause_ms / pauses.len() as f64 };
    s.max_pause_ms = pauses.iter().copied().fold(0.0, f64::max);
    s.p99_pause_ms = stats::percentile(&mut pauses, 99.0).unwrap_or(0.0);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const G1: &str = "\
[0.004s][info][gc] Using G1
[0.005s][info][gc,init] Version: 21.0.5+9-LTS (release)
[0.005s][info][gc,init] Heap Max Capacity: 8G
[1.234s][info][gc,start    ] GC(0) Pause Young (Normal) (G1 Evacuation Pause)
[1.239s][info][gc,heap     ] GC(0) Eden regions: 25->0(30)
[1.239s][info][gc,metaspace] GC(0) Metaspace: 9000K(9100K)->9000K(9100K) NonClass: 8000K(8100K)->8000K(8100K)
[1.240s][info][gc          ] GC(0) Pause Young (Normal) (G1 Evacuation Pause) 24M->8M(256M) 5.000ms
[1.240s][info][gc,cpu      ] GC(0) User=0.01s Sys=0.00s Real=0.01s
[9.000s][info][gc          ] GC(1) Pause Young (Concurrent Start) (G1 Humongous Allocation) 3100M->2000M(4096M) 15.000ms
[9.100s][info][gc          ] GC(1) Pause Remark 2100M->2050M(4096M) 2.000ms
[20.00s][info][gc          ] GC(2) Pause Full (System.gc()) 2500M->900M(4096M) 1.250s
";

    #[test]
    fn g1_pauses_and_heap() {
        let s = parse(G1);
        assert_eq!(s.collector, "G1");
        assert_eq!(s.java_version, "21.0.5+9-LTS");
        assert_eq!(s.max_heap_mb, 8192.0);
        assert_eq!(s.pauses, 4);
        assert_eq!(s.max_pause_ms, 1250.0);
        assert!((s.total_pause_ms - 1272.0).abs() < 1e-9);
        assert_eq!(s.avg_pause_ms, 318.0);
        // Metaspace's 9000K and Eden's region counts don't count as heap.
        assert_eq!(s.peak_heap_mb, 3100.0);
        assert_eq!(s.p99_pause_ms, 1250.0);
    }

    #[test]
    fn zgc_generational() {
        let log = "\
[0.010s][info][gc,init] Version: 21.0.2+13 (release)
[0.010s][info][gc     ] Using The Z Garbage Collector
[2.500s][info][gc,phases] GC(0) Y: Pause Mark Start (Major) 0.011ms
[2.600s][info][gc,phases] GC(0) Y: Pause Mark End 0.020ms
[2.700s][info][gc,phases] GC(0) Y: Pause Relocate Start 0.009ms
[2.800s][info][gc       ] GC(0) Major Collection (Warmup) 5200M(63%)->3800M(46%) 0.300s
[5.000s][info][gc       ] GC(1) Minor Collection (Allocation Rate) 1.5G(19%)->900M(11%) 0.050s
";
        let s = parse(log);
        assert_eq!(s.collector, "ZGC");
        assert_eq!(s.pauses, 3);
        assert!((s.max_pause_ms - 0.020).abs() < 1e-9);
        // The collection lines aren't pauses (they run alongside the game).
        assert_eq!(s.peak_heap_mb, 5200.0);
    }

    #[test]
    fn shenandoah_and_junk() {
        let log = "\
not a log line
[0.010s][info][gc] Using Shenandoah
[1.000s][info][gc] GC(0) Pause Init Mark (unload classes) 0.500ms
[1.100s][info][gc] GC(0) Concurrent marking (unload classes) 512M->600M(1024M) 80.000ms
[1.200s][info][gc] GC(0) Pause Final Mark (unload classes) 1.500ms
[1.200s][info][gc,start] GC(1) Pause Young (Normal)
";
        let s = parse(log);
        assert_eq!(s.collector, "Shenandoah");
        assert_eq!(s.pauses, 2);
        assert_eq!(s.max_pause_ms, 1.5);
        assert_eq!(s.peak_heap_mb, 512.0);
        assert_eq!(parse(""), GcStats::default());
    }

    #[test]
    fn line_parts() {
        assert_eq!(
            split_line("[1.5s][info][gc,start    ] GC(0) Pause Young"),
            Some((Some(1.5), "gc,start", "GC(0) Pause Young"))
        );
        assert_eq!(trailing_ms("x 0.080s"), Some(80.0));
        assert_eq!(trailing_ms("x 12us"), Some(0.012));
        assert_eq!(trailing_ms("Pause Young (Normal)"), None);
        assert_eq!(heap_before_mb("GC(1) Minor 1.5G(19%)->900M(11%) 0.050s"), Some(1536.0));
    }
}
