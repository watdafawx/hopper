//! Java arguments for a hosted server: memory and GC flag presets, kept in
//! `user_jvm_args.txt` for every loader (NeoForge and Forge read it from
//! their own run.bat; Hopper's run.bat for vanilla, Fabric and Quilt reads
//! it the same way). [`crate::server`] reads `-Xmx` from its own line here.

use std::fs;
use std::path::Path;

pub const ARGS_FILE: &str = "user_jvm_args.txt";

/// GC flag sets. Aikar's and ZGC are the same flags the client's Java
/// settings offer ([`crate::java::PRESETS`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JvmPreset {
    /// Aikar's tuned G1 flags: the usual choice for Minecraft servers.
    Aikar,
    /// No extra flags; the JVM's defaults.
    Plain,
    /// Generational ZGC, Java 21+: very short pauses, needs more memory.
    Zgc,
    /// Flags Hopper didn't write (detected when reading a file).
    Custom,
}

impl JvmPreset {
    pub const ALL: [JvmPreset; 3] = [JvmPreset::Aikar, JvmPreset::Plain, JvmPreset::Zgc];

    pub fn label(self) -> &'static str {
        match self {
            JvmPreset::Aikar => crate::java::PRESETS[0].0,
            JvmPreset::Plain => "Plain (JVM defaults)",
            JvmPreset::Zgc => crate::java::PRESETS[1].0,
            JvmPreset::Custom => "Custom",
        }
    }

    /// Whether `java_major` can run it.
    pub fn supported(self, java_major: u32) -> bool {
        self != JvmPreset::Zgc || java_major >= 21
    }

    /// The flags for `java_major`. Generational ZGC is opt-in on Java 21-22
    /// (`-XX:+ZGenerational`) and the only ZGC from 23 on, where the flag is
    /// deprecated (removed in 24).
    pub fn flags(self, java_major: u32) -> Vec<String> {
        let s = match self {
            JvmPreset::Aikar => crate::java::PRESETS[0].1,
            JvmPreset::Plain | JvmPreset::Custom => "",
            JvmPreset::Zgc => crate::java::PRESETS[1].1,
        };
        s.split_whitespace()
            .filter(|f| !(self == JvmPreset::Zgc && *f == "-XX:+ZGenerational" && java_major >= 23))
            .map(String::from)
            .collect()
    }

    /// Which preset `flags` are, by their collector.
    pub fn detect(flags: &[String]) -> JvmPreset {
        let has = |f: &str| flags.iter().any(|x| x == f);
        if flags.is_empty() {
            JvmPreset::Plain
        } else if has("-XX:+UseZGC") {
            JvmPreset::Zgc
        } else if has("-XX:+UseG1GC") && has("-XX:G1HeapRegionSize=8M") {
            JvmPreset::Aikar
        } else {
            JvmPreset::Custom
        }
    }
}

/// The parsed `user_jvm_args.txt`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JvmArgs {
    /// Comment lines (NeoForge's explanation header), kept on write.
    pub comments: Vec<String>,
    pub xms_mb: Option<u32>,
    pub xmx_mb: Option<u32>,
    /// Every other argument, in order.
    pub flags: Vec<String>,
}

impl JvmArgs {
    pub fn new(memory_mb: u32, preset: JvmPreset, java_major: u32) -> Self {
        JvmArgs {
            comments: Vec::new(),
            // Aikar: same min and max, so the heap never resizes.
            xms_mb: Some(memory_mb),
            xmx_mb: Some(memory_mb),
            flags: preset.flags(java_major),
        }
    }

    pub fn parse(text: &str) -> Self {
        let mut out = JvmArgs::default();
        for line in text.lines() {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if t.starts_with('#') {
                out.comments.push(line.trim_end().to_string());
                continue;
            }
            for tok in tokenize(t) {
                if let Some(m) = tok.strip_prefix("-Xmx").and_then(parse_mb) {
                    out.xmx_mb = Some(m);
                } else if let Some(m) = tok.strip_prefix("-Xms").and_then(parse_mb) {
                    out.xms_mb = Some(m);
                } else {
                    out.flags.push(tok);
                }
            }
        }
        out
    }

    pub fn load(root: &Path) -> anyhow::Result<Self> {
        match fs::read_to_string(root.join(ARGS_FILE)) {
            Ok(t) => Ok(Self::parse(&t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let path = root.join(ARGS_FILE);
        let tmp = path.with_extension("txt.hopper-tmp");
        fs::write(&tmp, self.to_text())?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Memory on their own lines first (where [`crate::server`] looks for
    /// `-Xmx`), then one flag per line.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for c in &self.comments {
            out.push_str(c);
            out.push('\n');
        }
        if let Some(m) = self.xms_mb {
            out.push_str(&format!("-Xms{}\n", format_mb(m)));
        }
        if let Some(m) = self.xmx_mb {
            out.push_str(&format!("-Xmx{}\n", format_mb(m)));
        }
        for f in &self.flags {
            out.push_str(&quote(f));
            out.push('\n');
        }
        out
    }

    /// Every argument, as Java gets them.
    pub fn args(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(m) = self.xms_mb {
            v.push(format!("-Xms{}", format_mb(m)));
        }
        if let Some(m) = self.xmx_mb {
            v.push(format!("-Xmx{}", format_mb(m)));
        }
        v.extend(self.flags.iter().cloned());
        v
    }

    pub fn preset(&self) -> JvmPreset {
        JvmPreset::detect(&self.flags)
    }

    /// Sets min and max heap to `mb`.
    pub fn set_memory(&mut self, mb: u32) {
        self.xms_mb = Some(mb);
        self.xmx_mb = Some(mb);
    }

    /// Replaces every `-XX:` flag with `preset`'s, keeping the rest (system
    /// properties like `-Dfoo=bar`, agents).
    pub fn set_preset(&mut self, preset: JvmPreset, java_major: u32) {
        self.flags.retain(|f| !f.starts_with("-XX:"));
        let mut new = preset.flags(java_major);
        new.append(&mut self.flags);
        self.flags = new;
    }
}

/// "12G" / "12288M" / "512m" / "4096" (bytes would be silly; MB assumed
/// only with a unit) -> MB.
pub fn parse_mb(s: &str) -> Option<u32> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num.parse().ok()?;
    let mb = match unit.to_ascii_lowercase().as_str() {
        "g" => n * 1024,
        "m" => n,
        "k" => n / 1024,
        "" => n / (1024 * 1024),
        _ => return None,
    };
    u32::try_from(mb).ok()
}

/// 12288 -> "12G", 3000 -> "3000M".
pub fn format_mb(mb: u32) -> String {
    if mb >= 1024 && mb % 1024 == 0 {
        format!("{}G", mb / 1024)
    } else {
        format!("{mb}M")
    }
}

/// Splits a Java argument-file line: whitespace separates, double or single
/// quotes group.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn quote(arg: &str) -> String {
    if arg.contains(char::is_whitespace) {
        format!("\"{arg}\"")
    } else {
        arg.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEOFORGE: &str = "# Xmx and Xms set the maximum and minimum RAM usage, respectively.\n# Uncomment the next line to set it.\n# -Xmx4G\n-Xmx12G\n-XX:+UseG1GC -Dfoo=\"a b\"\n";

    #[test]
    fn reads_and_writes_args_files() {
        let a = JvmArgs::parse(NEOFORGE);
        assert_eq!(a.xmx_mb, Some(12 * 1024));
        assert_eq!(a.xms_mb, None);
        assert_eq!(a.flags, ["-XX:+UseG1GC", "-Dfoo=a b"]);
        assert_eq!(a.comments.len(), 3);
        let text = a.to_text();
        assert!(text.starts_with("# Xmx and Xms"));
        assert!(text.contains("\n-Xmx12G\n"));
        assert!(text.contains("\"-Dfoo=a b\""));
        assert_eq!(JvmArgs::parse(&text), a);
        // crate::server's reader: a line starting with -Xmx.
        assert!(text.lines().any(|l| l.trim().strip_prefix("-Xmx") == Some("12G")));
    }

    #[test]
    fn presets_swap_gc_flags_and_keep_others() {
        let mut a = JvmArgs::new(6144, JvmPreset::Aikar, 21);
        assert_eq!(a.preset(), JvmPreset::Aikar);
        assert_eq!(a.args()[..2], ["-Xms6G".to_string(), "-Xmx6G".to_string()]);
        a.flags.push("-Dlog4j2.formatMsgNoLookups=true".into());
        a.set_preset(JvmPreset::Zgc, 21);
        assert_eq!(a.preset(), JvmPreset::Zgc);
        assert!(a.flags.contains(&"-XX:+ZGenerational".to_string()));
        assert!(a.flags.contains(&"-Dlog4j2.formatMsgNoLookups=true".to_string()));
        assert!(!a.flags.contains(&"-XX:+UseG1GC".to_string()));
        a.set_preset(JvmPreset::Plain, 21);
        assert_eq!(a.flags, ["-Dlog4j2.formatMsgNoLookups=true"]);
        assert!(!JvmPreset::Zgc.flags(25).contains(&"-XX:+ZGenerational".to_string()));
        assert!(!JvmPreset::Zgc.supported(17));
    }

    #[test]
    fn memory_units() {
        assert_eq!(parse_mb("12G"), Some(12288));
        assert_eq!(parse_mb("512m"), Some(512));
        assert_eq!(parse_mb("2048K"), Some(2));
        assert_eq!(parse_mb("x"), None);
        assert_eq!(format_mb(12288), "12G");
        assert_eq!(format_mb(3000), "3000M");
        assert_eq!(tokenize(r#"a "b c" '' d"#), ["a", "b c", "", "d"]);
    }
}
