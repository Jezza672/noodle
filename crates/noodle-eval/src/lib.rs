//! Warp quality metrics.
//!
//! Every metric compares a *reference* (what the warped audio should sound
//! like) with a *test* (what a warp method produced), and ignores phase,
//! because stretched audio is never phase-aligned with anything. The
//! reference's timeline maps onto the test's through a [`TimeMap`] the caller
//! supplies from the song's known source and target tempo maps; the maps are
//! never guessed from the audio, and never Noodle's own warp map.
//!
//! The spectral, phasiness and transient measures follow the features of
//! Roberts and Paliwal ("An Objective Measure of Quality for Time-Scale
//! Modification of Audio", JASA 2021), implemented from the paper's
//! descriptions; where the paper leaves a detail open the choice is noted on
//! the function. [`validate`] checks them against the authors' published
//! listening-test dataset.

pub mod clip;
pub mod level;
pub mod onsets;
pub mod phasiness;
pub mod pitch;
pub mod report;
pub mod spectral;
pub mod stereo;
pub mod stft;
pub mod timemap;
pub mod timing;
pub mod transient;
pub mod validate;

pub use clip::Clip;
pub use report::{Report, evaluate};
pub use timemap::TimeMap;
