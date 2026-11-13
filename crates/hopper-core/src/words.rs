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

/// "18 MB of 42 MB · 2 MB/s · about 10 s left": the bytes moved so far of
/// `total`, then the speed (bytes a second; 0 while it isn't known yet)
/// and a rough time left, once there is a speed to go by.
pub fn transfer(done: u64, total: u64, speed: f64) -> String {
    use crate::instance_ops::human_bytes;
    let mut s = human_bytes(done);
    if total > 0 {
        s.push_str(&format!(" of {}", human_bytes(total)));
    }
    if speed >= 1.0 {
        s.push_str(&format!(" · {}/s", human_bytes(speed as u64)));
        if total > done {
            let left = ((total - done) as f64 / speed).round() as u64;
            s.push_str(&match left {
                0..60 => format!(" · about {left} s left"),
                _ => format!(" · about {} min left", left.div_ceil(60)),
            });
        }
    }
    s
}

/// "Create, Jade, JEI and 4 more": the first `show` names, then how many
/// others there are.
pub fn names(list: &[String], show: usize) -> String {
    if list.len() <= show {
        return list.join(", ");
    }
    format!("{} and {} more", list[..show].join(", "), list.len() - show)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_says_size_speed_and_time_left() {
        const MB: u64 = 1024 * 1024;
        assert_eq!(transfer(18 * MB, 42 * MB, 0.0), "18 MB of 42 MB");
        assert_eq!(transfer(18 * MB, 42 * MB, 2.0 * MB as f64), "18 MB of 42 MB · 2 MB/s · about 12 s left");
        assert_eq!(transfer(0, 600 * MB, 1.0 * MB as f64), "0 KB of 600 MB · 1 MB/s · about 10 min left");
        // Done: no time left.
        assert_eq!(transfer(42 * MB, 42 * MB, 2.0 * MB as f64), "42 MB of 42 MB · 2 MB/s");
    }

    #[test]
    fn names_cut_long_lists() {
        let l: Vec<String> = ["Create", "Jade", "JEI", "Waystones", "Botania"].map(String::from).to_vec();
        assert_eq!(names(&l[..2], 3), "Create, Jade");
        assert_eq!(names(&l, 3), "Create, Jade, JEI and 2 more");
        assert_eq!(names(&[], 3), "");
    }

    #[test]
    fn one_is_singular_everything_else_plural() {
        assert_eq!(count(1, "file"), "1 file");
        assert_eq!(count(0, "file"), "0 files");
        assert_eq!(count(12, "mod"), "12 mods");
        assert_eq!(count_as(1, "copy", "copies"), "1 copy");
        assert_eq!(count_as(2, "copy", "copies"), "2 copies");
    }
}
