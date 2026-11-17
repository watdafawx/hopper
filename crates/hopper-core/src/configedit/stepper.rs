//! The number math behind the config editor's slider, the mouse wheel and
//! the - / + buttons, in one place so Hopper desktop and the Hopper Server
//! web UI step a setting the same way (the web UI mirrors `step_for` and
//! `stepped` in `ui/src/lib/stepper.js`).

use super::Value;

/// A setting's limits, as the file states them (either end may be open).
pub type Range = Option<(Option<f64>, Option<f64>)>;

/// How many decimals `x` needs (0.05 is 2, 3 is 0), up to 9.
fn decimals(x: f64) -> i32 {
    let mut d = 0;
    let mut y = x.abs();
    while d < 9 && (y - y.round()).abs() > 1e-9 * y.max(1.0) {
        y *= 10.0;
        d += 1;
    }
    d
}

/// `x` rounded to `d` decimals: takes the float noise out (0.1 + 0.2).
pub fn round_to(x: f64, d: i32) -> f64 {
    let m = 10f64.powi(d.clamp(0, 9));
    (x * m).round() / m
}

/// The slider's step for a float setting spanning `span`: a 1 or 5 times a
/// power of ten, at most a hundredth of the span (so the thumb has a
/// hundred places or more, and every one is a tidy number: 0.01 for 0..1,
/// 0.1 for 0..16, 50 for 0..5000).
pub fn slider_step(span: f64) -> f64 {
    if !(span.is_finite() && span > 0.0) {
        return 1.0;
    }
    let target = span / 100.0;
    let pow = 10f64.powf(target.log10().floor());
    let step = if target >= 5.0 * pow { 5.0 * pow } else { pow };
    // `10f64.powf(-2.0)` is 0.010000000000000002: keep the step clean.
    round_to(step, (-step.log10().floor()).max(0.0) as i32 + 1)
}

/// The step one notch of the wheel (and a - / + click) moves a value by,
/// before Shift multiplies it: 1 for a whole number; for a float, the
/// slider's step when the range is known, else by how big the value is.
pub fn step_for(value: &Value, range: Range) -> f64 {
    match value {
        Value::Int(_) => 1.0,
        _ => {
            if let Some((Some(lo), Some(hi))) = range
                && hi > lo
                && hi - lo <= 100_000.0
            {
                return slider_step(hi - lo);
            }
            let f = value.number().unwrap_or(0.0).abs();
            if f >= 10.0 {
                1.0
            } else if f >= 1.0 || f == 0.0 {
                0.1
            } else {
                0.01
            }
        }
    }
}

fn clamp(x: f64, range: Range) -> f64 {
    let Some((lo, hi)) = range else { return x };
    x.max(lo.unwrap_or(f64::MIN)).min(hi.unwrap_or(f64::MAX))
}

/// `value` moved by `notches` steps (positive: up), stopping at the ends of
/// `range` rather than refusing. A whole number stays whole; a float keeps
/// the decimals of its step and of itself, with no float noise. Anything
/// that isn't a number comes back as it was.
pub fn stepped(value: &Value, notches: f64, step: f64, range: Range) -> Value {
    match value {
        Value::Int(i) => {
            let by = (notches * step).round() as i64;
            let moved = i.saturating_add(by) as f64;
            Value::Int(clamp(moved, range).round() as i64)
        }
        Value::Float(f) => {
            let d = decimals(step).max(decimals(*f));
            Value::Float(clamp(round_to(f + notches * step, d), range))
        }
        other => other.clone(),
    }
}

/// The setting's value for a slider at `x` (lo..hi): snapped to `step` from
/// the low end, inside the range, a whole number for an integer setting.
pub fn from_slider(x: f64, lo: f64, hi: f64, step: f64, whole: bool) -> Value {
    let x = x.clamp(lo, hi);
    if whole {
        return Value::Int(x.round() as i64);
    }
    let step = if step > 0.0 { step } else { 1.0 };
    // The top is always reachable, even when it isn't on the step grid.
    if hi - x < step / 2.0 {
        return Value::Float(hi);
    }
    let snapped = lo + ((x - lo) / step).round() * step;
    let d = decimals(step).max(decimals(lo));
    Value::Float(round_to(snapped, d).clamp(lo, hi))
}

/// A number typed in a plain field (a port, a count of days, GB), moved by
/// `notches` steps (positive: up) and given back as text, stopping at `lo`
/// and `hi`. Text that isn't a number counts as the low end (or 0), so the
/// first notch up from an empty field gives the lowest value plus a step.
pub fn step_text(text: &str, notches: f64, step: f64, lo: Option<f64>, hi: Option<f64>, whole: bool) -> String {
    let range = Some((lo, hi));
    let t = text.trim();
    let start = if whole {
        Value::Int(t.parse::<i64>().ok().or_else(|| t.parse::<f64>().ok().map(|f| f.round() as i64)).unwrap_or(lo.unwrap_or(0.0) as i64))
    } else {
        Value::Float(t.parse::<f64>().ok().filter(|f| f.is_finite()).unwrap_or(lo.unwrap_or(0.0)))
    };
    match stepped(&start, notches, step, range) {
        Value::Int(i) => i.to_string(),
        Value::Float(f) => super::num_text(f),
        other => other.text(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: fn(f64, f64) -> Range = |a, b| Some((Some(a), Some(b)));

    #[test]
    fn slider_steps_are_tidy_numbers() {
        assert_eq!(slider_step(1.0), 0.01);
        assert_eq!(slider_step(2.0), 0.01);
        assert_eq!(slider_step(3.0), 0.01);
        assert_eq!(slider_step(16.0), 0.1);
        assert_eq!(slider_step(100.0), 1.0);
        assert_eq!(slider_step(255.0), 1.0);
        assert_eq!(slider_step(500.0), 5.0);
        assert_eq!(slider_step(5000.0), 50.0);
        assert_eq!(slider_step(0.0), 1.0);
        assert_eq!(slider_step(f64::NAN), 1.0);
        // At least a hundred places on the track.
        for span in [0.5, 1.0, 7.0, 16.0, 99.0, 250.0, 3000.0, 100_000.0] {
            assert!(span / slider_step(span) >= 99.9, "{span}");
        }
    }

    #[test]
    fn the_step_for_a_value() {
        assert_eq!(step_for(&Value::Int(5), None), 1.0);
        assert_eq!(step_for(&Value::Float(0.35), R(0.0, 1.0)), 0.01);
        assert_eq!(step_for(&Value::Float(2.5), R(0.0, 16.0)), 0.1);
        // No range: by size.
        assert_eq!(step_for(&Value::Float(250.0), None), 1.0);
        assert_eq!(step_for(&Value::Float(2.5), None), 0.1);
        assert_eq!(step_for(&Value::Float(0.0), Some((Some(0.0), None))), 0.1);
        assert_eq!(step_for(&Value::Float(0.25), None), 0.01);
        // A huge span is no slider: by size.
        assert_eq!(step_for(&Value::Float(5.0), R(0.0, 1e9)), 0.1);
    }

    #[test]
    fn a_whole_number_steps_and_stops_at_the_ends() {
        assert_eq!(stepped(&Value::Int(196), 1.0, 1.0, R(0.0, 255.0)), Value::Int(197));
        assert_eq!(stepped(&Value::Int(196), -2.0, 1.0, R(0.0, 255.0)), Value::Int(194));
        assert_eq!(stepped(&Value::Int(255), 1.0, 1.0, R(0.0, 255.0)), Value::Int(255));
        assert_eq!(stepped(&Value::Int(0), -1.0, 1.0, R(0.0, 255.0)), Value::Int(0));
        // Shift: ten at a time, still stops at the end.
        assert_eq!(stepped(&Value::Int(250), 10.0, 1.0, R(0.0, 255.0)), Value::Int(255));
        assert_eq!(stepped(&Value::Int(3), -10.0, 1.0, R(0.0, 255.0)), Value::Int(0));
        // Open ends.
        assert_eq!(stepped(&Value::Int(-5), -1.0, 1.0, None), Value::Int(-6));
        assert_eq!(stepped(&Value::Int(5), 1.0, 1.0, Some((Some(1.0), None))), Value::Int(6));
        assert_eq!(stepped(&Value::Int(i64::MAX), 1.0, 1.0, None), Value::Int(i64::MAX));
    }

    #[test]
    fn a_float_steps_without_noise() {
        let mut v = Value::Float(0.0);
        for _ in 0..3 {
            v = stepped(&v, 1.0, 0.1, R(0.0, 1.0));
        }
        assert_eq!(v, Value::Float(0.3));
        assert_eq!(stepped(&Value::Float(0.35), 1.0, 0.01, R(0.0, 1.0)), Value::Float(0.36));
        assert_eq!(stepped(&Value::Float(0.99), 5.0, 0.01, R(0.0, 1.0)), Value::Float(1.0));
        assert_eq!(stepped(&Value::Float(0.02), -5.0, 0.01, R(0.0, 1.0)), Value::Float(0.0));
        // A value with more decimals than the step keeps them.
        assert_eq!(stepped(&Value::Float(0.125), 1.0, 0.1, None), Value::Float(0.225));
        assert_eq!(stepped(&Value::Float(1234.5), 1.0, 50.0, R(0.0, 5000.0)), Value::Float(1284.5));
        // Not a number: untouched.
        assert_eq!(stepped(&Value::Bool(true), 1.0, 1.0, None), Value::Bool(true));
    }

    #[test]
    fn the_slider_snaps_and_clamps() {
        assert_eq!(from_slider(116.4, 0.0, 255.0, 1.0, true), Value::Int(116));
        assert_eq!(from_slider(300.0, 0.0, 255.0, 1.0, true), Value::Int(255));
        assert_eq!(from_slider(-3.0, 0.0, 255.0, 1.0, true), Value::Int(0));
        assert_eq!(from_slider(0.3500002, 0.0, 1.0, 0.01, false), Value::Float(0.35));
        assert_eq!(from_slider(0.999, 0.0, 1.0, 0.01, false), Value::Float(1.0));
        assert_eq!(from_slider(2.56, 0.0, 16.0, 0.1, false), Value::Float(2.6));
        // From the low end, not from zero.
        assert_eq!(from_slider(-0.47, -1.0, 1.0, 0.05, false), Value::Float(-0.45));
        assert_eq!(from_slider(1234.0, 0.0, 5000.0, 50.0, false), Value::Float(1250.0));
        // A top that isn't on the grid is still reachable.
        assert_eq!(from_slider(16.0, 0.0, 16.0, 0.3, false), Value::Float(16.0));
        // 0 or 1.
        assert_eq!(from_slider(0.6, 0.0, 1.0, 1.0, true), Value::Int(1));
    }

    #[test]
    fn a_typed_number_steps_as_text() {
        assert_eq!(step_text("15", 1.0, 1.0, Some(1.0), Some(1440.0), true), "16");
        assert_eq!(step_text(" 15 ", -1.0, 1.0, Some(1.0), Some(1440.0), true), "14");
        assert_eq!(step_text("1", -1.0, 1.0, Some(1.0), Some(1440.0), true), "1");
        assert_eq!(step_text("65535", 1.0, 1.0, Some(1.0), Some(65535.0), true), "65535");
        assert_eq!(step_text("8799", 10.0, 1.0, Some(1.0), Some(65535.0), true), "8809");
        // Empty or not a number: from the low end.
        assert_eq!(step_text("", 1.0, 1.0, Some(0.0), None, true), "1");
        assert_eq!(step_text("abc", -1.0, 1.0, Some(0.0), None, true), "0");
        assert_eq!(step_text("", 1.0, 1.0, None, None, true), "1");
        // GB: decimals stay, no trailing ".0".
        assert_eq!(step_text("1.5", 1.0, 0.5, Some(0.0), Some(100_000.0), false), "2");
        assert_eq!(step_text("0", -1.0, 0.5, Some(0.0), None, false), "0");
        assert_eq!(step_text("0.1", 2.0, 0.1, Some(0.0), None, false), "0.3");
    }
}
