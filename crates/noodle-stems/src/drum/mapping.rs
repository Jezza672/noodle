//! From a model's parts onto the drum rack's pads, so what reads a separation
//! (the analyser, #501) is the same whichever model made it.

use noodle_io::DrumKind;

use crate::drum::DrumPart;

/// The pads a part can feed, best fit first. A part that is not drums on its
/// own (the remainder) feeds the hats and cymbals.
pub fn pads_for(part: DrumPart) -> &'static [DrumKind] {
    match part {
        DrumPart::Kick => &[DrumKind::Kick],
        DrumPart::Snare => &[DrumKind::Snare],
        DrumPart::Toms => &[DrumKind::LowTom, DrumKind::HighTom],
        DrumPart::HiHat => &[DrumKind::ClosedHat, DrumKind::OpenHat],
        DrumPart::Cymbals => &[DrumKind::Crash],
        DrumPart::Rest => &[DrumKind::ClosedHat, DrumKind::OpenHat, DrumKind::Crash],
    }
}

/// The part of `available` to listen to for a pad, if any: the part made for
/// it, else the remainder. A clap, or an unknown hit, has no part (no model
/// separates claps).
pub fn part_for(kind: DrumKind, available: &[DrumPart]) -> Option<DrumPart> {
    let wanted: &[DrumPart] = match kind {
        DrumKind::Kick => &[DrumPart::Kick],
        DrumKind::Snare => &[DrumPart::Snare],
        DrumKind::LowTom | DrumKind::HighTom => &[DrumPart::Toms],
        DrumKind::ClosedHat | DrumKind::OpenHat => &[DrumPart::HiHat, DrumPart::Rest],
        DrumKind::Crash => &[DrumPart::Cymbals, DrumPart::Rest],
        DrumKind::Clap | DrumKind::Unknown => &[],
    };
    wanted.iter().copied().find(|p| available.contains(p))
}

/// Every pad kind with the part that serves it, for a model that returns
/// `available`. `None` is a pad the model cannot help with.
pub fn pad_sources(available: &[DrumPart]) -> Vec<(DrumKind, Option<DrumPart>)> {
    [
        DrumKind::Kick,
        DrumKind::Snare,
        DrumKind::ClosedHat,
        DrumKind::OpenHat,
        DrumKind::Clap,
        DrumKind::LowTom,
        DrumKind::HighTom,
        DrumKind::Crash,
    ]
    .into_iter()
    .map(|k| (k, part_for(k, available)))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drum::{ALL_MODELS, DRUMSEP, LARSNET, TIMBRICA};

    #[test]
    fn larsnet_covers_every_pad_but_the_clap() {
        let sources = pad_sources(LARSNET.parts);
        for (kind, part) in sources {
            assert_eq!(part.is_none(), kind == DrumKind::Clap, "{kind:?}");
        }
    }

    #[test]
    fn timbrica_hats_and_cymbals_come_from_the_remainder() {
        for kind in [DrumKind::ClosedHat, DrumKind::OpenHat, DrumKind::Crash] {
            assert_eq!(part_for(kind, TIMBRICA.parts), Some(DrumPart::Rest));
        }
        assert_eq!(
            part_for(DrumKind::Kick, TIMBRICA.parts),
            Some(DrumPart::Kick)
        );
    }

    #[test]
    fn drumsep_has_no_hat_pad_source() {
        assert_eq!(part_for(DrumKind::ClosedHat, DRUMSEP.parts), None);
        assert_eq!(
            part_for(DrumKind::Crash, DRUMSEP.parts),
            Some(DrumPart::Cymbals)
        );
        assert_eq!(
            part_for(DrumKind::HighTom, DRUMSEP.parts),
            Some(DrumPart::Toms)
        );
    }

    #[test]
    fn a_part_is_used_only_for_pads_it_lists() {
        for m in ALL_MODELS {
            for (kind, part) in pad_sources(m.parts) {
                if let Some(part) = part {
                    assert!(pads_for(part).contains(&kind), "{} {kind:?} {part}", m.id);
                }
            }
        }
    }
}
