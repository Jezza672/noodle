//! Group G of the scorecard: does it hold up?
//!
//! - **Glitch-free.** Output underruns the engine counted while the route and
//!   its late changes ran. Reported, not asserted: a loaded CI machine can
//!   drop a block that no change of ours caused.
//! - **Save and reopen.** After the route, the project is saved and opened
//!   again. It must read back identically, and the route's done check must
//!   still pass on it.

/// What saving and opening the project again did.
#[derive(Clone, Debug, PartialEq)]
pub enum Reopen {
    /// The project read back identically and the done check passes.
    Same,
    /// The project could not be saved.
    CannotSave,
    /// The reopened project differs from the one saved: the first line that
    /// does, as saved then as read.
    Differs(String),
    /// The reopened project is identical but the done check fails on it.
    DoneFails(String),
}

impl Reopen {
    pub fn ok(&self) -> bool {
        *self == Reopen::Same
    }

    pub fn summary(&self) -> String {
        match self {
            Reopen::Same => "identical".into(),
            Reopen::CannotSave => "CANNOT SAVE".into(),
            Reopen::Differs(why) => format!("DIFFERS {why}"),
            Reopen::DoneFails(why) => format!("DONE CHECK FAILS {why}"),
        }
    }
}

/// Compares the project as it was saved with the same project read back.
/// The first line that differs, if any.
pub fn first_difference(saved: &str, reopened: &str) -> Option<String> {
    if saved == reopened {
        return None;
    }
    let mut a = saved.lines();
    let mut b = reopened.lines();
    let mut line = 1;
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) if x == y => line += 1,
            (None, None) => return Some("in how the file ends".into()),
            (x, y) => {
                let cut = |s: Option<&str>| {
                    let s = s.unwrap_or("(nothing)").trim();
                    s.chars().take(80).collect::<String>()
                };
                return Some(format!(
                    "at line {line}: saved {:?}, read back {:?}",
                    cut(x),
                    cut(y)
                ));
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Robustness {
    /// Underruns after the route and its late changes, or `None` if the
    /// engine never ran.
    pub underruns: Option<u64>,
    pub reopen: Reopen,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_has_no_difference() {
        assert_eq!(first_difference("a\nb\n", "a\nb\n"), None);
    }

    #[test]
    fn the_first_changed_line_is_named() {
        let d = first_difference("a\nb\nc\n", "a\nx\nc\n").unwrap();
        assert!(
            d.contains("line 2") && d.contains("\"b\"") && d.contains("\"x\""),
            "{d}"
        );
    }

    #[test]
    fn a_lost_line_is_named() {
        let d = first_difference("a\nb\n", "a\n").unwrap();
        assert!(d.contains("line 2") && d.contains("(nothing)"), "{d}");
    }

    #[test]
    fn only_an_identical_project_that_passes_is_fine() {
        assert!(Reopen::Same.ok());
        assert!(!Reopen::CannotSave.ok());
        assert!(!Reopen::Differs("x".into()).ok());
        assert!(!Reopen::DoneFails("x".into()).ok());
    }
}
