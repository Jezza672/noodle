//! What each note-on that went through a node became, so the note-off (and
//! the expression) can follow it, shared by the nodes that change or multiply
//! notes (Transpose and Chord).
//!
//! A note-off goes to what its note-on turned into, not to what the node's
//! parameters would make of it now, so turning a parameter while a note is
//! held never strands it.

/// The most notes one note-on becomes.
pub(crate) const MAX_OUT: usize = 8;
/// How many sounding notes are remembered. Past this the oldest is
/// forgotten.
const NOTES: usize = 256;

/// One note a note-on became: its ID, and how late it was played, in
/// samples (its note-off is as late).
#[derive(Clone, Copy, Default)]
pub(crate) struct Out {
    pub id: u32,
    pub delay: u32,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Held {
    note: u32,
    live: bool,
    len: u8,
    outs: [Out; MAX_OUT],
}

impl Held {
    /// The notes it became; none if it was dropped.
    pub fn outs(&self) -> &[Out] {
        &self.outs[..usize::from(self.len)]
    }
}

pub(crate) struct HeldNotes {
    slots: Box<[Held]>,
    next: usize,
}

impl HeldNotes {
    pub fn new() -> Self {
        Self {
            slots: vec![Held::default(); NOTES].into_boxed_slice(),
            next: 0,
        }
    }

    pub fn clear(&mut self) {
        self.slots.fill(Held::default());
        self.next = 0;
    }

    /// Records that `note` became `outs` (at most [`MAX_OUT`]; none means it
    /// was dropped). A note already held under the same ID is replaced.
    pub fn insert(&mut self, note: u32, outs: &[Out]) {
        // The same note again, else a free slot, and only when every slot
        // holds a sounding note the oldest is forgotten.
        let at = match self.slots.iter().position(|h| h.live && h.note == note) {
            Some(at) => at,
            None => match (0..NOTES)
                .map(|k| (self.next + k) % NOTES)
                .find(|&at| !self.slots[at].live)
            {
                Some(at) => {
                    self.next = (at + 1) % NOTES;
                    at
                }
                None => {
                    let at = self.next;
                    self.next = (self.next + 1) % NOTES;
                    at
                }
            },
        };
        let len = outs.len().min(MAX_OUT);
        let mut held = Held {
            note,
            live: true,
            len: len as u8,
            outs: [Out::default(); MAX_OUT],
        };
        held.outs[..len].copy_from_slice(&outs[..len]);
        self.slots[at] = held;
    }

    /// What `note` became, if it is held.
    pub fn get(&self, note: u32) -> Option<&Held> {
        self.slots.iter().find(|h| h.live && h.note == note)
    }

    /// What `note` became, forgetting it: its note-off has come.
    pub fn release(&mut self, note: u32) -> Option<Held> {
        let held = self.slots.iter_mut().find(|h| h.live && h.note == note)?;
        held.live = false;
        Some(*held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_note_survives_more_notes_than_the_table_holds() {
        let mut held = HeldNotes::new();
        held.insert(1, &[Out { id: 1, delay: 0 }]);
        for note in 2..2 * NOTES as u32 {
            held.insert(note, &[Out { id: note, delay: 0 }]);
            held.release(note);
        }
        assert_eq!(held.get(1).map(|h| h.outs().len()), Some(1));
    }

    #[test]
    fn a_full_table_forgets_the_oldest() {
        let mut held = HeldNotes::new();
        for note in 0..NOTES as u32 + 1 {
            held.insert(note, &[Out { id: note, delay: 0 }]);
        }
        assert!(held.get(0).is_none());
        assert!(held.get(NOTES as u32).is_some());
    }
}
