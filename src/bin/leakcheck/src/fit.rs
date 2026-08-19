//! Least-squares fit of counter value against iteration index.
//!
//! Slope, not delta. A before/after difference around one operation cannot separate a one-time
//! cache fill from a per-iteration leak -- at N=2 the two are identical. The slope of the tail can:
//! caching contributes a step, a leak contributes a gradient that persists however long you run.

pub struct Fit {
    /// Units per iteration.
    pub slope: f64,
    /// Fraction of variance explained. A steep slope with low r2 is churn, not a leak.
    pub r2: f64,
    /// Value at the end of the tail minus value at its start.
    pub growth: f64,
    /// Fraction of iterations at which the counter rose at all.
    pub duty: f64,
    /// The largest single increase as a fraction of total growth.
    ///
    /// This is what separates a leak from background activity, and r2 does not. A counter that
    /// climbs in two jumps of four fits a line about as well as one that climbs by one every
    /// iteration -- the null control does exactly that -- but the first is something else in the
    /// system doing a piece of work and the second is the operation under test retaining a unit
    /// per call. A leak spreads its growth evenly, so no single step dominates.
    pub max_step_frac: f64,
    pub n: usize,
}

/// Fit `ys[i]` against `i`. Returns `None` for a series too short or containing the
/// absent sentinel, so a failed gate call is never fitted as a step change.
pub fn fit(ys: &[u64]) -> Option<Fit> {
    let n = ys.len();
    if n < 3 || ys.iter().any(|&y| y == u64::MAX) {
        return None;
    }
    let nf = n as f64;
    let xbar = (n - 1) as f64 / 2.0;
    let ybar = ys.iter().map(|&y| y as f64).sum::<f64>() / nf;

    let mut sxy = 0.0;
    let mut sxx = 0.0;
    let mut syy = 0.0;
    for (i, &y) in ys.iter().enumerate() {
        let dx = i as f64 - xbar;
        let dy = y as f64 - ybar;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    if sxx == 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    // A perfectly flat series explains nothing and needs to explain nothing: call it r2 = 1 rather
    // than 0/0, or every clean counter reports as unexplained noise.
    let r2 = if syy == 0.0 { 1.0 } else { (sxy * sxy) / (sxx * syy) };

    let growth = ys[n - 1] as f64 - ys[0] as f64;
    let mut rises = 0usize;
    let mut max_step = 0i64;
    for w in ys.windows(2) {
        let d = w[1] as i64 - w[0] as i64;
        if d > 0 {
            rises += 1;
            max_step = max_step.max(d);
        }
    }
    let steps = (n - 1) as f64;

    Some(Fit {
        slope,
        r2,
        growth,
        duty: rises as f64 / steps,
        max_step_frac: if growth > 0.0 { max_step as f64 / growth } else { 0.0 },
        n,
    })
}
