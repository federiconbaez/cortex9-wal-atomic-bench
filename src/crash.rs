//! Crash harness: random inserts and updates with periodic checkpoints, a
//! crash during one page write of a checkpoint flush, recovery, checks.

use std::collections::BTreeMap;

use crate::engine::{value_of, CrashPlan, Engine, Mode};
use crate::page::VAL;
use crate::prereg::Prereg;
use crate::rng::Rng;

/// One crash trial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrashResult {
    /// Pages torn or not redoable after recovery.
    pub torn_pages: u64,
    /// Recovered contents differ from the committed state (or the tree is
    /// unreadable).
    pub wrong_state: bool,
    /// Page writes in the trial's checkpoints.
    pub writes: u64,
    /// Pages failing their CRC on disk right after the crash, before
    /// recovery (the tears recovery has to deal with). All-zero pages are a
    /// new page whose first write never landed, not a tear.
    pub torn_on_disk: u64,
}

enum Op {
    Insert(u64),
    Update(u64),
}

struct Plan {
    keys: Vec<u64>,
    ops: Vec<Op>,
}

fn plan(p: &Prereg, seed: u64) -> Plan {
    let mut r = Rng::new(seed);
    let n0 = p.int("crash.initial_keys");
    let mut set = std::collections::BTreeSet::new();
    while (set.len() as u64) < n0 {
        set.insert(r.next_u64() >> 1);
    }
    let keys: Vec<u64> = set.iter().copied().collect();
    let mut live = keys.clone();
    let mut ops = Vec::new();
    for _ in 0..p.int("crash.ops_per_trial") {
        if r.chance(p.get("crash.insert_fraction")) {
            let mut k = r.next_u64() >> 1;
            while set.contains(&k) {
                k = r.next_u64() >> 1;
            }
            set.insert(k);
            live.push(k);
            ops.push(Op::Insert(k));
        } else {
            ops.push(Op::Update(
                live[r.between(0, live.len() as u64 - 1) as usize],
            ));
        }
    }
    Plan { keys, ops }
}

fn execute(
    mode: Mode,
    p: &Prereg,
    pl: &Plan,
    crash: Option<&CrashPlan>,
) -> (Engine, BTreeMap<u64, [u8; VAL]>) {
    let mut e = Engine::bulk_load(
        mode,
        &pl.keys,
        p.get("tree.leaf_fill"),
        p.int("bytes.fpi_hole_elimination") == 1,
        true,
    );
    e.initial_flush();
    let mut model: BTreeMap<u64, [u8; VAL]> =
        pl.keys.iter().map(|&k| (k, value_of(k, 0))).collect();
    let every = p.int("crash.checkpoint_every_ops");
    for (i, op) in pl.ops.iter().enumerate() {
        let v = value_of(i as u64, i as u64 + 1);
        match *op {
            Op::Insert(k) => {
                assert!(e.insert(k, v));
                model.insert(k, v);
            }
            Op::Update(k) => {
                assert!(e.update(k, v));
                model.insert(k, v);
            }
        }
        if (i as u64 + 1) % every == 0 && e.checkpoint(crash).is_err() {
            return (e, model);
        }
    }
    (e, model)
}

/// Run trial `seed` in `mode`.
pub fn run(p: &Prereg, mode: Mode, seed: u64) -> CrashResult {
    let pl = plan(p, seed);
    // Dry run to count crashable page writes.
    let (dry, _) = execute(mode, p, &pl, None);
    let writes = dry.writes;
    assert!(writes > 0, "seed {seed}: no checkpoint page writes");
    let mut r = Rng::new(seed ^ 0xC4A5_11ED);
    let cp = CrashPlan {
        at_write: r.between(0, writes - 1),
        bits: r.next_u64(),
        persist_p: p.get("crash.tear_unit_persist_p"),
        unit: p.int("crash.tear_unit_bytes") as usize,
    };
    let (e, model) = execute(mode, p, &pl, Some(&cp));
    let torn_on_disk = e
        .disk
        .as_ref()
        .expect("durable")
        .iter()
        .filter(|pg| !crate::page::verify(&pg[..]) && pg.iter().any(|&b| b != 0))
        .count() as u64;
    let (rec, rep) = e.recover();
    let wrong_state = match rec.scan() {
        Some(m) => m != model,
        None => true,
    };
    CrashResult {
        torn_pages: rep.torn_pages,
        wrong_state,
        writes,
        torn_on_disk,
    }
}

/// Totals over seeds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CrashTally {
    /// Trials.
    pub trials: u64,
    /// Torn pages after recovery, summed.
    pub torn_pages: u64,
    /// Trials with at least one torn page.
    pub trials_with_torn: u64,
    /// Trials whose recovered state is wrong.
    pub wrong_state: u64,
    /// Torn pages on disk before recovery, summed.
    pub torn_on_disk: u64,
}

/// Run `count` seeds from `start` on `threads` threads.
pub fn run_many(p: &Prereg, mode: Mode, start: u64, count: u64, threads: usize) -> CrashTally {
    let threads = threads.max(1) as u64;
    std::thread::scope(|sc| {
        let hs: Vec<_> = (0..threads)
            .map(|t| {
                sc.spawn(move || {
                    let mut tl = CrashTally::default();
                    let mut s = start + t;
                    while s < start + count {
                        let r = run(p, mode, s);
                        tl.trials += 1;
                        tl.torn_pages += r.torn_pages;
                        tl.trials_with_torn += u64::from(r.torn_pages > 0);
                        tl.wrong_state += u64::from(r.wrong_state);
                        tl.torn_on_disk += r.torn_on_disk;
                        s += threads;
                    }
                    tl
                })
            })
            .collect();
        hs.into_iter().fold(CrashTally::default(), |mut a, h| {
            let t = h.join().expect("worker");
            a.trials += t.trials;
            a.torn_pages += t.torn_pages;
            a.trials_with_torn += t.trials_with_torn;
            a.wrong_state += t.wrong_state;
            a.torn_on_disk += t.torn_on_disk;
            a
        })
    })
}
