//! Engine, crash harness and bytes bench on small sizes and development
//! seeds (outside the registered ranges).

use std::collections::BTreeMap;

use wal_atomic_bench::bench;
use wal_atomic_bench::crash;
use wal_atomic_bench::engine::{value_of, Engine, Mode};
use wal_atomic_bench::prereg::Prereg;
use wal_atomic_bench::rng::Rng;

#[test]
fn inserts_with_splits_keep_every_key() {
    let mut e = Engine::bulk_load(Mode::Fpi, &[10, 20, 30], 0.9, true, false);
    let mut model: BTreeMap<u64, _> = [10u64, 20, 30]
        .iter()
        .map(|&k| (k, value_of(k, 0)))
        .collect();
    let mut r = Rng::new(5);
    for i in 0..20_000u64 {
        let k = r.next_u64() >> 1;
        let v = value_of(k, i);
        if e.insert(k, v) {
            model.insert(k, v);
        }
    }
    assert!(e.pages.len() > 100, "splits must have happened");
    e.seal_all();
    assert_eq!(e.scan().expect("readable tree"), model);
    for (k, v) in model.iter().take(500) {
        assert_eq!(e.get(*k), Some(*v));
    }
}

#[test]
fn recovery_after_clean_checkpoints_is_exact_in_every_mode() {
    for mode in [Mode::Fpi, Mode::Atomic, Mode::Unprotected] {
        let keys: Vec<u64> = (0..3000).map(|i| i * 7).collect();
        let mut e = Engine::bulk_load(mode, &keys, 0.9, true, true);
        e.initial_flush();
        let mut model: BTreeMap<u64, _> = keys.iter().map(|&k| (k, value_of(k, 0))).collect();
        let mut r = Rng::new(9);
        for i in 0..3000u64 {
            if r.chance(0.3) {
                let k = (r.next_u64() >> 1) | 1;
                let v = value_of(k, i + 1);
                if e.insert(k, v) {
                    model.insert(k, v);
                }
            } else {
                let k = keys[r.between(0, 2999) as usize];
                let v = value_of(k, i + 1);
                assert!(e.update(k, v));
                model.insert(k, v);
            }
            if (i + 1) % 400 == 0 {
                e.checkpoint(None).unwrap();
            }
        }
        // Crash without a checkpoint at the end: redo must replay the tail.
        let (rec, rep) = e.recover();
        assert_eq!(rep.torn_pages, 0, "{mode:?}");
        assert_eq!(rec.scan().expect("readable"), model, "{mode:?}");
    }
}

#[test]
fn crash_harness_on_dev_seeds_matches_expectations() {
    let p = Prereg::embedded();
    let f = crash::run_many(&p, Mode::Fpi, 9_100_000, 60, 2);
    let a = crash::run_many(&p, Mode::Atomic, 9_100_000, 60, 2);
    let u = crash::run_many(&p, Mode::Unprotected, 9_100_000, 60, 2);
    assert_eq!((f.torn_pages, f.wrong_state), (0, 0), "{f:?}");
    assert_eq!((a.torn_pages, a.wrong_state), (0, 0), "{a:?}");
    assert!(f.torn_on_disk > 0, "FPI mode must have had tears to repair");
    assert_eq!(a.torn_on_disk, 0, "atomic writes never tear");
    assert!(u.torn_pages > 0 && u.wrong_state > 0, "{u:?}");
}

#[test]
fn bytes_bench_small_fpi_costs_more() {
    let p = Prereg::embedded();
    let r = bench::run(&p, 901, 500, 10_000, 5_000);
    assert!(r.fpi_bytes_per_tx > r.atomic_bytes_per_tx);
    // Without FPIs every update is one 129-byte record plus checkpoints.
    assert!(r.atomic_bytes_per_tx < 130.0, "{r:?}");
    let (m, lo, hi) = bench::bootstrap_ci(&[0.5, 0.6, 0.55, 0.52, 0.58], 2000, 1);
    assert!(lo <= m && m <= hi);
}

#[cfg(target_os = "linux")]
#[test]
fn statx_detection_runs() {
    let s = wal_atomic_bench::detect::query(std::path::Path::new(".")).expect("statx works");
    if !s.reported {
        assert_eq!((s.unit_min, s.unit_max), (0, 0));
    }
}
