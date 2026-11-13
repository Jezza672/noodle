//! Decibel conversion shared by the nodes.

/// The linear gain of `db` decibels.
pub(crate) fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// The root mean square of `x`, for tests.
#[cfg(test)]
pub(crate) fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}
