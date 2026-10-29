//! CLI for the pre-registered measurements.

use std::process::ExitCode;
use std::time::Instant;

use wal_atomic_bench::bench::{bootstrap_ci, run as bytes_run};
use wal_atomic_bench::crash::run_many;
use wal_atomic_bench::detect::query;
use wal_atomic_bench::engine::Mode;
use wal_atomic_bench::prereg::Prereg;

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  wal-atomic-bench bytes [--interval small|primary|large|all]\n  wal-atomic-bench crash --mode fpi|atomic|unprotected [--threads N] [--shard I/N]\n  wal-atomic-bench detect [PATH]\n  wal-atomic-bench dev"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().cloned() else {
        return usage();
    };
    let p = Prereg::embedded();
    let mut threads = 4usize;
    let mut shard = (0u64, 1u64);
    let mut mode = None;
    let mut interval = "all".to_string();
    let mut path = None;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--threads" => threads = it.next().and_then(|v| v.parse().ok()).unwrap_or(4),
            "--interval" => interval = it.next().cloned().unwrap_or_default(),
            "--mode" => {
                mode = match it.next().map(|s| s.as_str()) {
                    Some("fpi") => Some(Mode::Fpi),
                    Some("atomic") => Some(Mode::Atomic),
                    Some("unprotected") => Some(Mode::Unprotected),
                    _ => return usage(),
                }
            }
            "--shard" => {
                let Some((i, n)) = it.next().and_then(|v| v.split_once('/')) else {
                    return usage();
                };
                match (i.parse(), n.parse()) {
                    (Ok(i), Ok(n)) if i < n => shard = (i, n),
                    _ => return usage(),
                }
            }
            other if cmd == "detect" && path.is_none() => path = Some(other.to_string()),
            _ => return usage(),
        }
    }
    match cmd.as_str() {
        "bytes" => {
            let keys = p.int("bytes.keys");
            let txs = p.int("bytes.transactions");
            let runs = p.int("bytes.runs");
            let s0 = p.int("bytes.runs_seed_start");
            let names: Vec<&str> = match interval.as_str() {
                "all" => vec!["small", "primary", "large"],
                "small" | "primary" | "large" => vec![interval.as_str()],
                _ => return usage(),
            };
            let mut ok = true;
            for name in names {
                let every = p.int(&format!("bytes.checkpoint_{name}"));
                let t0 = Instant::now();
                let rs: Vec<_> = (s0..s0 + runs)
                    .map(|s| bytes_run(&p, s, every, keys, txs))
                    .collect();
                let red: Vec<f64> = rs.iter().map(|r| r.reduction()).collect();
                let (m, lo, hi) = bootstrap_ci(&red, p.int("bytes.bootstrap_resamples"), 12345);
                let fpi: f64 = rs.iter().map(|r| r.fpi_bytes_per_tx).sum::<f64>() / runs as f64;
                let atm: f64 = rs.iter().map(|r| r.atomic_bytes_per_tx).sum::<f64>() / runs as f64;
                println!(
                    "interval={name} checkpoint_every_tx={every} keys={keys} txs={txs} runs={runs} fpi_bytes_per_tx={fpi:.1} atomic_bytes_per_tx={atm:.1} reduction_mean={:.4}% ci95=[{:.4}%, {:.4}%] per_run={:?} wall_s={:.1}",
                    100.0 * m,
                    100.0 * lo,
                    100.0 * hi,
                    rs.iter().map(|r| (r.seed, (r.fpi_bytes_per_tx * 10.0).round() / 10.0, (r.atomic_bytes_per_tx * 10.0).round() / 10.0, r.fpis)).collect::<Vec<_>>(),
                    t0.elapsed().as_secs_f64()
                );
                if name == "primary" && lo < p.get("thresholds.wal_bytes_reduction_min") {
                    eprintln!("FAIL: CI lower bound {lo} below threshold");
                    ok = false;
                }
            }
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        "crash" => {
            let Some(mode) = mode else {
                return usage();
            };
            let s0 = p.int("crash.seeds_start");
            let total = p.int("crash.crashes_per_mode");
            let per = total.div_ceil(shard.1);
            let start = s0 + shard.0 * per;
            let count = per.min(s0 + total - start);
            let t0 = Instant::now();
            let t = run_many(&p, mode, start, count, threads);
            println!(
                "mode={mode:?} seeds={start}..{} trials={} torn_on_disk_before_recovery={} torn_pages={} trials_with_torn={} wrong_state={} wall_s={:.1}",
                start + count,
                t.trials,
                t.torn_on_disk,
                t.torn_pages,
                t.trials_with_torn,
                t.wrong_state,
                t0.elapsed().as_secs_f64()
            );
            let ok = match mode {
                Mode::Fpi => {
                    t.torn_pages <= p.int("thresholds.fpi_torn_pages_max") && t.wrong_state == 0
                }
                Mode::Atomic => {
                    t.torn_pages <= p.int("thresholds.atomic_emulated_torn_pages_max")
                        && t.wrong_state == 0
                }
                Mode::Unprotected => t.torn_pages >= p.int("thresholds.control_torn_pages_min"),
            };
            if ok {
                ExitCode::SUCCESS
            } else {
                eprintln!("FAIL: {mode:?} outside its threshold");
                ExitCode::FAILURE
            }
        }
        "detect" => {
            let path = path.unwrap_or_else(|| ".".into());
            match query(std::path::Path::new(&path)) {
                Ok(s) => println!(
                    "path={path} statx_write_atomic_reported={} unit_min={} unit_max={} segments_max={} rwf_atomic_usable_for_16k_pages={}",
                    s.reported,
                    s.unit_min,
                    s.unit_max,
                    s.segments_max,
                    s.reported && s.unit_max >= 16384
                ),
                Err(e) => println!("path={path} statx_error={e}"),
            }
            ExitCode::SUCCESS
        }
        "dev" => {
            // Development sizes and seeds, outside the registered ranges.
            let r = bytes_run(&p, 900, 1000, 20_000, 20_000);
            println!(
                "dev bytes (not registered): {r:?} reduction={:.4}",
                r.reduction()
            );
            for m in [Mode::Fpi, Mode::Atomic, Mode::Unprotected] {
                let t = run_many(&p, m, 9_000_000, 500, threads);
                println!("dev crash (not registered) {m:?}: {t:?}");
            }
            ExitCode::SUCCESS
        }
        _ => usage(),
    }
}
