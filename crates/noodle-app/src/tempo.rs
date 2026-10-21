//! Edits to the session's tempo map: the tempo field in the transport and the
//! tempo lane in the arrangement. Each takes the map and gives the map it
//! should become (or `None` if that is no change or not a valid map); the
//! caller makes it a [`Command::SetTempoMap`](noodle_core::Command).

use noodle_core::{SignatureChange, TempoChange, TempoMap, Tick, TimeSignature};

/// Two tempos closer than this are the same.
const SAME_BPM: f64 = 1e-6;

/// The map with `bpm` from the tempo change that holds at `tick` to the next:
/// what the transport's tempo field edits (the only tempo, or the stretch
/// the playhead is in).
pub fn with_tempo_at(map: &TempoMap, tick: Tick, bpm: f64) -> Option<TempoMap> {
    let mut tempos = map.tempos().to_vec();
    let i = tempos.partition_point(|t| t.tick <= tick).saturating_sub(1);
    if (tempos[i].bpm - bpm).abs() < SAME_BPM {
        return None;
    }
    tempos[i].bpm = bpm;
    TempoMap::new(tempos, map.signatures().to_vec()).ok()
}

/// The map with a tempo change at `tick` of `bpm`, replacing one already
/// there. Returns the map and the place of the change among the changes.
pub fn with_change(map: &TempoMap, tick: Tick, bpm: f64) -> Option<(TempoMap, usize)> {
    let mut tempos = map.tempos().to_vec();
    let at = tempos.partition_point(|t| t.tick < tick);
    if tempos.get(at).is_some_and(|t| t.tick == tick) {
        tempos[at].bpm = bpm;
    } else {
        tempos.insert(at, TempoChange { tick, bpm });
    }
    let new = TempoMap::new(tempos, map.signatures().to_vec()).ok()?;
    Some((new, at))
}

/// The map with tempo change `index` moved to `tick` and given `bpm`. The
/// first change stays at the start, and none may cross its neighbours.
pub fn moved(map: &TempoMap, index: usize, tick: Tick, bpm: f64) -> Option<TempoMap> {
    let mut tempos = map.tempos().to_vec();
    let last = tempos.len().checked_sub(1)?;
    if index > last {
        return None;
    }
    let tick = if index == 0 {
        Tick::ZERO
    } else {
        // At least a tick clear of each neighbour.
        let low = tempos[index - 1].tick.0 + 1;
        let high = tempos
            .get(index + 1)
            .map_or(i64::MAX, |next| next.tick.0 - 1);
        if low > high {
            return None;
        }
        Tick(tick.0.clamp(low, high))
    };
    tempos[index] = TempoChange { tick, bpm };
    let new = TempoMap::new(tempos, map.signatures().to_vec()).ok()?;
    (new != *map).then_some(new)
}

/// The map without tempo change `index`. The first can't be removed.
pub fn removed(map: &TempoMap, index: usize) -> Option<TempoMap> {
    if index == 0 || index >= map.tempos().len() {
        return None;
    }
    let mut tempos = map.tempos().to_vec();
    tempos.remove(index);
    TempoMap::new(tempos, map.signatures().to_vec()).ok()
}

/// The map in `signature` from the start, whatever it was before.
pub fn with_signature(map: &TempoMap, signature: TimeSignature) -> Option<TempoMap> {
    let mut signatures = map.signatures().to_vec();
    if signatures[0].signature == signature {
        return None;
    }
    signatures[0].signature = signature;
    TempoMap::new(map.tempos().to_vec(), signatures).ok()
}

/// The map in `signature` from the start of `bar` on, until the next
/// change. A change already at `bar` is replaced. Returns the map and the
/// place of the change among the signature changes, or `None` if the
/// signature already holds at `bar`.
pub fn with_signature_at(
    map: &TempoMap,
    bar: u32,
    signature: TimeSignature,
) -> Option<(TempoMap, usize)> {
    let mut signatures = map.signatures().to_vec();
    let at = signatures.partition_point(|s| s.bar < bar);
    if signatures.get(at).is_some_and(|s| s.bar == bar) {
        signatures[at].signature = signature;
    } else {
        if signatures[at - 1].signature == signature {
            return None;
        }
        signatures.insert(at, SignatureChange { bar, signature });
    }
    let new = TempoMap::new(map.tempos().to_vec(), signatures).ok()?;
    (new != *map).then_some((new, at))
}

/// The map without time signature change `index`. The first can't be removed.
pub fn signature_removed(map: &TempoMap, index: usize) -> Option<TempoMap> {
    if index == 0 || index >= map.signatures().len() {
        return None;
    }
    let mut signatures = map.signatures().to_vec();
    signatures.remove(index);
    TempoMap::new(map.tempos().to_vec(), signatures).ok()
}

/// The time signatures offered in the transport.
pub const SIGNATURES: [(u8, u8); 7] = [(4, 4), (3, 4), (2, 4), (5, 4), (6, 8), (7, 8), (12, 8)];

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> TempoMap {
        TempoMap::new(
            vec![
                TempoChange {
                    tick: Tick(0),
                    bpm: 120.0,
                },
                TempoChange {
                    tick: Tick(3840),
                    bpm: 90.0,
                },
            ],
            TempoMap::default().signatures().to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn the_tempo_field_edits_the_stretch_the_playhead_is_in() {
        let m = map();
        let early = with_tempo_at(&m, Tick(100), 140.0).unwrap();
        assert_eq!(early.bpm_at(Tick(0)), 140.0);
        assert_eq!(early.bpm_at(Tick(4000)), 90.0);
        let late = with_tempo_at(&m, Tick(3840), 100.0).unwrap();
        assert_eq!(late.bpm_at(Tick(0)), 120.0);
        assert_eq!(late.bpm_at(Tick(4000)), 100.0);
        assert_eq!(with_tempo_at(&m, Tick(0), 120.0), None, "no change");
        assert_eq!(with_tempo_at(&m, Tick(0), 5.0), None, "out of range");
    }

    #[test]
    fn a_change_is_added_in_order_or_replaces_the_one_there() {
        let m = map();
        let (added, at) = with_change(&m, Tick(1920), 100.0).unwrap();
        assert_eq!(at, 1);
        assert_eq!(added.tempos().len(), 3);
        let (replaced, at) = with_change(&m, Tick(3840), 70.0).unwrap();
        assert_eq!((replaced.tempos().len(), at), (2, 1));
        assert_eq!(replaced.bpm_at(Tick(3840)), 70.0);
        assert!(with_change(&m, Tick(960), 2000.0).is_none());
    }

    #[test]
    fn a_change_moves_between_its_neighbours_and_the_first_stays_put() {
        let m = with_change(&map(), Tick(1920), 100.0).unwrap().0;
        // Pulled past the next change, it stops a tick short of it.
        let moved_far = moved(&m, 1, Tick(9000), 100.0).unwrap();
        assert_eq!(moved_far.tempos()[1].tick, Tick(3839));
        let back = moved(&m, 1, Tick(-50), 100.0).unwrap();
        assert_eq!(back.tempos()[1].tick, Tick(1));
        // The first change keeps the start whatever it is asked.
        let first = moved(&m, 0, Tick(500), 130.0).unwrap();
        assert_eq!(first.tempos()[0].tick, Tick(0));
        assert_eq!(first.tempos()[0].bpm, 130.0);
        assert_eq!(moved(&m, 1, Tick(1920), 100.0), None, "no change");
        assert_eq!(moved(&m, 9, Tick(1920), 100.0), None);
    }

    #[test]
    fn the_first_change_cant_be_removed() {
        let m = map();
        assert_eq!(removed(&m, 0), None);
        assert_eq!(removed(&m, 1).unwrap().tempos().len(), 1);
        assert_eq!(removed(&m, 5), None);
    }

    #[test]
    fn the_signature_changes_from_the_start() {
        let m = map();
        let sig = TimeSignature {
            numerator: 3,
            denominator: 4,
        };
        let new = with_signature(&m, sig).unwrap();
        assert_eq!(new.signatures()[0].signature, sig);
        assert_eq!(new.tempos(), m.tempos());
        assert_eq!(with_signature(&new, sig), None);
    }

    #[test]
    fn a_signature_change_is_added_in_order_or_replaces_the_one_there() {
        let three = TimeSignature {
            numerator: 3,
            denominator: 4,
        };
        let six = TimeSignature {
            numerator: 6,
            denominator: 8,
        };
        let m = map();
        let (added, at) = with_signature_at(&m, 4, three).unwrap();
        assert_eq!((added.signatures().len(), at), (2, 1));
        assert_eq!(added.signatures()[1].bar, 4);
        let (later, at) = with_signature_at(&added, 8, six).unwrap();
        assert_eq!((later.signatures().len(), at), (3, 2));
        let (replaced, at) = with_signature_at(&later, 4, six).unwrap();
        assert_eq!((replaced.signatures().len(), at), (3, 1));
        assert_eq!(replaced.signatures()[1].signature, six);
        // The signature already holding there is no change; bar 0 replaces
        // the first.
        assert_eq!(with_signature_at(&m, 4, TimeSignature::COMMON), None);
        assert_eq!(with_signature_at(&added, 4, three), None);
        let (first, at) = with_signature_at(&m, 0, three).unwrap();
        assert_eq!((first.signatures().len(), at), (1, 0));
        assert_eq!(first.signatures()[0].signature, three);
    }

    #[test]
    fn the_first_signature_cant_be_removed() {
        let three = TimeSignature {
            numerator: 3,
            denominator: 4,
        };
        let (m, _) = with_signature_at(&map(), 4, three).unwrap();
        assert_eq!(signature_removed(&m, 0), None);
        assert_eq!(signature_removed(&m, 1).unwrap().signatures().len(), 1);
        assert_eq!(signature_removed(&m, 5), None);
    }
}
