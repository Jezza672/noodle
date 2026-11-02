//! Group B of the scorecard: do I keep doing the same thing?
//!
//! A route that scales (samples, tracks, takes) is run at two sizes. The
//! extra actions and KLM seconds, divided by the extra items, say what one
//! more item costs. About 0 means one gesture covers any number; a fixed
//! cost per item means the user does the chore by hand each time.

use super::effort::Effort;

#[derive(Clone, Debug, PartialEq)]
pub struct PerItem {
    pub small: usize,
    pub large: usize,
    /// Extra actions per extra item.
    pub actions: f64,
    /// Extra KLM seconds per extra item.
    pub klm_seconds: f64,
}

/// What each item beyond `small` costs, from the efforts at the two sizes.
pub fn per_item(small: (usize, &Effort), large: (usize, &Effort)) -> Option<PerItem> {
    let (small_n, small_e) = small;
    let (large_n, large_e) = large;
    if large_n <= small_n {
        return None;
    }
    let extra = (large_n - small_n) as f64;
    Some(PerItem {
        small: small_n,
        large: large_n,
        actions: (large_e.actions as f64 - small_e.actions as f64) / extra,
        klm_seconds: (large_e.klm_seconds - small_e.klm_seconds) / extra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effort(actions: usize, klm: f64) -> Effort {
        Effort {
            actions,
            klm_seconds: klm,
            ..Effort::default()
        }
    }

    #[test]
    fn a_chore_done_by_hand_costs_its_actions_per_item() {
        let p = per_item((3, &effort(10, 20.0)), (24, &effort(73, 83.0))).unwrap();
        assert_eq!(p.actions, 3.0);
        assert_eq!(p.klm_seconds, 3.0);
    }

    #[test]
    fn one_gesture_for_any_number_costs_nothing_per_item() {
        let p = per_item((3, &effort(7, 9.0)), (24, &effort(7, 9.0))).unwrap();
        assert_eq!(p.actions, 0.0);
        assert_eq!(p.klm_seconds, 0.0);
    }

    #[test]
    fn two_equal_sizes_say_nothing() {
        assert_eq!(per_item((3, &effort(1, 1.0)), (3, &effort(1, 1.0))), None);
    }
}
