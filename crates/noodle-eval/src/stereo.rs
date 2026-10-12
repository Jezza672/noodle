//! Stereo image: channel correlation and width.

use crate::clip::Clip;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Stereo {
    pub reference_correlation: f64,
    pub test_correlation: f64,
    pub correlation_delta: f64,
    /// Side energy over mid energy, in dB, test minus reference.
    pub width_delta_db: f64,
}

/// `None` unless both clips are stereo.
pub fn compare(reference: Clip, test: Clip) -> Option<Stereo> {
    if reference.channels != 2 || test.channels != 2 {
        return None;
    }
    let (rc, rw) = image(reference);
    let (tc, tw) = image(test);
    Some(Stereo {
        reference_correlation: rc,
        test_correlation: tc,
        correlation_delta: tc - rc,
        width_delta_db: tw - rw,
    })
}

/// (Pearson correlation of left and right, side-over-mid energy in dB.)
fn image(c: Clip) -> (f64, f64) {
    let (mut ll, mut rr, mut lr, mut mm, mut ss) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for f in c.samples.chunks_exact(2) {
        let (l, r) = (f64::from(f[0]), f64::from(f[1]));
        ll += l * l;
        rr += r * r;
        lr += l * r;
        mm += (l + r) * (l + r) * 0.25;
        ss += (l - r) * (l - r) * 0.25;
    }
    let corr = if ll > 0.0 && rr > 0.0 {
        lr / (ll * rr).sqrt()
    } else {
        f64::NAN
    };
    (corr, 10.0 * (ss.max(1e-30) / mm.max(1e-30)).log10())
}
