//! Small pieces of the wording both apps show.

/// "1 file", "3 files": `n` and the word for one, with an "s" unless it's one.
/// For words whose plural isn't just an "s", use [`count_as`].
pub fn count(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// "1 mod is", "2 mods are": `n` with the word for one and for many.
pub fn count_as(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_is_singular_everything_else_plural() {
        assert_eq!(count(1, "file"), "1 file");
        assert_eq!(count(0, "file"), "0 files");
        assert_eq!(count(12, "mod"), "12 mods");
        assert_eq!(count_as(1, "copy", "copies"), "1 copy");
        assert_eq!(count_as(2, "copy", "copies"), "2 copies");
    }
}
