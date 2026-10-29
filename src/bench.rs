//! WAL bytes per transaction, FPI vs atomic page writes.

use crate::engine::{value_of, Engine, Mode};
use crate::prereg::Prereg;
use crate::rng::Rng;

/// One run.
#[derive(Debug, Clone, Copy)]
pub struct BytesRun {
    /// Seed.
    pub seed: u64,
    /// Checkpoint interval (transactions).
    pub checkpoint_every: u64,
    /// WAL bytes per transaction with FPI.
    pub fpi_bytes_per_tx: f64,
    /// WAL bytes per transaction without FPI.
    pub atomic_bytes_per_tx: f64,
    /// FPIs logged in FPI mode.
    pub fpis: u64,
}

impl BytesRun {
    /// 1 - atomic / fpi.
    pub fn reduction(&self) -> f64 {
        1.0 - self.atomic_bytes_per_tx / self.fpi_bytes_per_tx
    }
}

fn one(mode: Mode, p: &Prereg, keys: u64, txs: u64, every: u64, seed: u64) -> (f64, u64) {
    let ks: Vec<u64> = (0..keys).map(|i| i * 1000).collect();
    let mut e = Engine::bulk_load(
        mode,
        &ks,
        p.get("tree.leaf_fill"),
        p.int("bytes.fpi_hole_elimination") == 1,
        false,
    );
    let mut r = Rng::new(seed);
    e.wal_bytes = 0;
    for t in 0..txs {
        let k = r.between(0, keys - 1) * 1000;
        assert!(e.update(k, value_of(k, t + 1)));
        if (t + 1) % every == 0 {
            e.checkpoint(None).expect("no crash plan");
        }
    }
    (e.wal_bytes as f64 / txs as f64, e.fpis)
}

/// Run both modes for one seed and interval. `keys`/`txs` override the
/// registered sizes (tests only).
pub fn run(p: &Prereg, seed: u64, every: u64, keys: u64, txs: u64) -> BytesRun {
    let (f, fpis) = one(Mode::Fpi, p, keys, txs, every, seed);
    let (a, _) = one(Mode::Atomic, p, keys, txs, every, seed);
    BytesRun {
        seed,
        checkpoint_every: every,
        fpi_bytes_per_tx: f,
        atomic_bytes_per_tx: a,
        fpis,
    }
}

/// Percentile bootstrap 95% CI of the mean.
pub fn bootstrap_ci(xs: &[f64], resamples: u64, seed: u64) -> (f64, f64, f64) {
    let n = xs.len();
    let mean = xs.iter().sum::<f64>() / n as f64;
    let mut r = Rng::new(seed);
    let mut means: Vec<f64> = (0..resamples)
        .map(|_| {
            (0..n)
                .map(|_| xs[r.between(0, n as u64 - 1) as usize])
                .sum::<f64>()
                / n as f64
        })
        .collect();
    means.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let lo = means[((resamples as f64) * 0.025) as usize];
    let hi = means[(((resamples as f64) * 0.975) as usize).min(resamples as usize - 1)];
    (mean, lo, hi)
}
