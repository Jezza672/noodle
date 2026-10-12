//! Warp quality metrics.
//!
//! Every metric compares a *reference* (what the warped audio should sound
//! like) with a *test* (what a warp method produced), and ignores phase,
//! because stretched audio is never phase-aligned with anything. The
//! reference's timeline maps onto the test's through a [`TimeMap`] the caller
//! supplies from the song's known source and target tempo maps; the maps are
//! never guessed from the audio, and never Noodle's own warp map.
//!
//! [`paper`] holds the features of Roberts and Paliwal ("An Objective Measure
//! of Quality for Time-Scale Modification of Audio", JASA 2021) as the paper
//! defines them, under its names. The other modules are Noodle's own
//! diagnostics, which say more about *what* went wrong. [`validate`] checks
//! all of them against the authors' published listening-test dataset.

pub mod borrowed;
pub mod clip;
pub mod harness;
pub mod level;
pub mod onsets;
pub mod paper;
pub mod phasiness;
pub mod pitch;
pub mod probe;
pub mod refs;
pub mod report;
pub mod song;
pub mod spectral;
pub mod stereo;
pub mod stft;
pub mod table;
pub mod tempo;
pub mod timemap;
pub mod timing;
pub mod transient;
pub mod validate;

pub use clip::Clip;
pub use report::{Report, evaluate};
pub use timemap::TimeMap;
