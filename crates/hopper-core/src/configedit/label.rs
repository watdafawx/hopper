//! Friendly names for setting keys: `fanParticleDensity` reads as "Fan
//! particle density", `max_belt_length` as "Max belt length". The raw key
//! still shows under it and stays searchable.

/// `key` as a short sentence-case name.
pub fn label(key: &str) -> String {
    // A dotted key names its last part (`fluidFogSettings.honey`).
    let key = key.trim_matches('"');
    let key = key.rsplit('.').next().filter(|s| !s.is_empty()).unwrap_or(key);
    let chars: Vec<char> = key.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if matches!(c, '_' | '-' | ' ' | '.') {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1).copied();
        let split = match prev {
            None => false,
            Some(p) if !cur.is_empty() => {
                // camelCase, digits after letters, and the end of an acronym
                // (`FEOutput` -> FE Output).
                (c.is_uppercase() && p.is_lowercase())
                    || (c.is_ascii_digit() && p.is_lowercase())
                    || (c.is_alphabetic() && p.is_ascii_digit() && c.is_uppercase())
                    || (c.is_uppercase() && p.is_uppercase() && next.is_some_and(char::is_lowercase))
            }
            _ => false,
        };
        if split {
            words.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        // Acronyms stay as they are (FE, AE2, JEI); a SHOUTED_KEY reads as words.
        let all_caps = w.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase);
        let shouted = words.len() > 1 && words.iter().all(|w| w.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase));
        let w = if all_caps && w.chars().count() > 1 && !shouted { w.clone() } else { w.to_lowercase() };
        if i == 0 {
            let mut cs = w.chars();
            if let Some(f) = cs.next() {
                out.extend(f.to_uppercase());
                out.push_str(cs.as_str());
            }
        } else {
            out.push_str(&w);
        }
    }
    if out.is_empty() { key.to_string() } else { out }
}

#[cfg(test)]
mod tests {
    use super::label;

    #[test]
    fn names() {
        assert_eq!(label("fanParticleDensity"), "Fan particle density");
        assert_eq!(label("max_belt_length"), "Max belt length");
        assert_eq!(label("enable-tooltips"), "Enable tooltips");
        assert_eq!(label("enableFEOutput"), "Enable FE output");
        assert_eq!(label("fluidFogSettings.honey"), "Honey");
        assert_eq!(label("MAX_SIZE"), "Max size");
        assert_eq!(label("AE2Channels"), "AE2 channels");
        assert_eq!(label("level2Cap"), "Level 2 cap");
        assert_eq!(label("\"Quoted Key\""), "Quoted key");
        assert_eq!(label("x"), "X");
    }
}
