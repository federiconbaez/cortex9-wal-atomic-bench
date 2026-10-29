//! B+tree with a WAL, checkpoints, tearable page writes and redo.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::page::{self, Page, INTERNAL, LEAF, PAGE, VAL};

/// WAL protection mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Full page image on the first change after a checkpoint's redo
    /// pointer; page writes may tear.
    Fpi,
    /// No FPI; page writes are atomic (emulated).
    Atomic,
    /// Positive control: no FPI and page writes may tear.
    Unprotected,
}

impl Mode {
    /// Logs FPIs.
    pub fn fpi(self) -> bool {
        self == Mode::Fpi
    }
    /// Page writes are atomic.
    pub fn atomic(self) -> bool {
        self == Mode::Atomic
    }
}

/// WAL record header bytes: lsn(8) len(4) crc(4) type(1) page(4).
pub const REC_HDR: u64 = 21;

/// WAL record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rec {
    /// Set the value of an existing key in a leaf.
    Update {
        /// Page.
        page: u32,
        /// Key.
        key: u64,
        /// Value.
        val: [u8; VAL],
    },
    /// Insert a key into a leaf that has room.
    Insert {
        /// Page.
        page: u32,
        /// Key.
        key: u64,
        /// Value.
        val: [u8; VAL],
    },
    /// Page image (FPI or split). `hole` = free-space range omitted.
    Image {
        /// Page.
        page: u32,
        /// Bytes as logged (hole zeroed if eliminated).
        data: Page,
        /// Omitted hole length.
        hole: usize,
    },
    /// Root changed.
    NewRoot {
        /// New root page.
        page: u32,
    },
    /// Checkpoint started; redo may begin here.
    Checkpoint {
        /// Redo pointer.
        redo: u64,
    },
}

impl Rec {
    /// Encoded size in bytes.
    pub fn size(&self) -> u64 {
        REC_HDR
            + match self {
                Rec::Update { .. } | Rec::Insert { .. } => 8 + VAL as u64,
                Rec::Image { hole, .. } => 4 + (PAGE - hole) as u64,
                Rec::NewRoot { .. } => 0,
                Rec::Checkpoint { .. } => 8,
            }
    }
}

/// Control file (written atomically after a checkpoint completes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    /// Redo start LSN.
    pub redo: u64,
    /// Root page.
    pub root: u32,
}

/// Crash injection for page writes.
#[derive(Debug, Clone, Copy)]
pub struct CrashPlan {
    /// Crash during the page write with this index (counting writes in
    /// checkpoints after the initial one).
    pub at_write: u64,
    /// Bits deciding which 4 KiB units (or the whole page) persist.
    pub bits: u64,
    /// Persist probability per unit, as a threshold on 16-bit draws.
    pub persist_p: f64,
    /// Unit size.
    pub unit: usize,
}

/// The crash happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crashed;

/// Engine state.
pub struct Engine {
    /// Mode.
    pub mode: Mode,
    /// Buffer pool: every page in memory.
    pub pages: Vec<Page>,
    /// Root.
    pub root: u32,
    dirty: BTreeSet<u32>,
    fpi_done: HashSet<u32>,
    next_lsn: u64,
    /// WAL bytes written since the counter was reset.
    pub wal_bytes: u64,
    /// Records kept for redo (crash harness only).
    pub wal: Option<Vec<(u64, Rec)>>,
    hole_elimination: bool,
    /// Durable pages (crash harness only).
    pub disk: Option<Vec<Page>>,
    /// Durable control file.
    pub control: Control,
    /// Page writes done in checkpoints after the initial one.
    pub writes: u64,
    /// FPI count.
    pub fpis: u64,
}

fn val_from(k: u64, salt: u64) -> [u8; VAL] {
    let mut v = [0u8; VAL];
    let mut x = k ^ salt.rotate_left(29) ^ 0xA5A5_5A5A_1234_5678;
    for b in v.iter_mut() {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (x >> 56) as u8;
    }
    v
}

/// Deterministic value for key `k` version `salt`.
pub fn value_of(k: u64, salt: u64) -> [u8; VAL] {
    val_from(k, salt)
}

impl Engine {
    /// Bulk-load sorted `keys` (values from `value_of(k, 0)`) with leaf fill
    /// `fill`, then take the initial checkpoint. `durable` keeps a disk and
    /// the WAL records for crash recovery.
    pub fn bulk_load(
        mode: Mode,
        keys: &[u64],
        fill: f64,
        hole_elimination: bool,
        durable: bool,
    ) -> Self {
        let mut pages: Vec<Page> = Vec::new();
        let per_leaf = (((PAGE - page::HDR) / (page::LEAF_REC + 2)) as f64 * fill).floor() as usize;
        let per_int = (((PAGE - page::HDR) / (page::INT_REC + 2)) as f64 * fill).floor() as usize;
        let mut level: Vec<(u64, u32)> = Vec::new();
        for chunk in keys.chunks(per_leaf.max(1)) {
            let pid = pages.len() as u32;
            let mut p = page::new_page(pid, LEAF);
            let recs: Vec<(u64, Vec<u8>)> = chunk
                .iter()
                .map(|&k| (k, value_of(k, 0).to_vec()))
                .collect();
            page::rebuild(&mut p[..], &recs);
            level.push((chunk[0], pid));
            pages.push(p);
        }
        if level.is_empty() {
            let mut p = page::new_page(0, LEAF);
            page::rebuild(&mut p[..], &[]);
            pages.push(p);
            level.push((0, 0));
        }
        while level.len() > 1 {
            let mut up = Vec::new();
            for chunk in level.chunks(per_int.max(2) + 1) {
                let pid = pages.len() as u32;
                let mut p = page::new_page(pid, INTERNAL);
                page::set_leftmost(&mut p[..], chunk[0].1);
                let recs: Vec<(u64, Vec<u8>)> = chunk[1..]
                    .iter()
                    .map(|&(k, c)| (k, c.to_le_bytes().to_vec()))
                    .collect();
                page::rebuild(&mut p[..], &recs);
                up.push((chunk[0].0, pid));
                pages.push(p);
            }
            level = up;
        }
        let root = level[0].1;
        for p in pages.iter_mut() {
            page::seal(&mut p[..]);
        }
        let disk = durable.then(|| pages.clone());
        Engine {
            mode,
            pages,
            root,
            dirty: BTreeSet::new(),
            fpi_done: HashSet::new(),
            next_lsn: 1,
            wal_bytes: 0,
            wal: durable.then(Vec::new),
            hole_elimination,
            disk,
            control: Control { redo: 1, root },
            writes: 0,
            fpis: 0,
        }
    }

    fn log(&mut self, r: Rec) -> u64 {
        let lsn = self.next_lsn;
        self.next_lsn += 1;
        self.wal_bytes += r.size();
        if let Some(w) = self.wal.as_mut() {
            w.push((lsn, r));
        }
        lsn
    }

    fn image(&mut self, pid: u32) -> u64 {
        let p = &self.pages[pid as usize];
        let mut data = p.clone();
        let hole = if self.hole_elimination {
            let (lo, hi) = page::used_ranges(&p[..]);
            data[lo..hi].fill(0);
            hi - lo
        } else {
            0
        };
        let lsn = self.log(Rec::Image {
            page: pid,
            data,
            hole,
        });
        self.fpi_done.insert(pid);
        page::set_lsn(&mut self.pages[pid as usize][..], lsn);
        self.dirty.insert(pid);
        lsn
    }

    fn path_to_leaf(&self, k: u64) -> Vec<u32> {
        let mut path = vec![self.root];
        loop {
            let p = &self.pages[*path.last().unwrap() as usize];
            if page::kind(&p[..]) == LEAF {
                return path;
            }
            path.push(page::route(&p[..], k));
        }
    }

    /// Look up `k`.
    pub fn get(&self, k: u64) -> Option<[u8; VAL]> {
        let leaf = *self.path_to_leaf(k).last().unwrap();
        let p = &self.pages[leaf as usize];
        page::find(&p[..], k)
            .ok()
            .map(|i| page::value(&p[..], i).try_into().unwrap())
    }

    /// Update an existing key (one transaction). Returns false if missing.
    pub fn update(&mut self, k: u64, v: [u8; VAL]) -> bool {
        let leaf = *self.path_to_leaf(k).last().unwrap();
        let Ok(i) = page::find(&self.pages[leaf as usize][..], k) else {
            return false;
        };
        page::set_value(&mut self.pages[leaf as usize][..], i, &v);
        self.after_change(
            leaf,
            Rec::Update {
                page: leaf,
                key: k,
                val: v,
            },
        );
        true
    }

    fn after_change(&mut self, pid: u32, r: Rec) {
        if self.mode.fpi() && !self.fpi_done.contains(&pid) {
            // The image carries the change.
            self.fpis += 1;
            self.image(pid);
        } else {
            let lsn = self.log(r);
            page::set_lsn(&mut self.pages[pid as usize][..], lsn);
            self.dirty.insert(pid);
        }
    }

    /// Insert a new key (one transaction). Returns false if it exists.
    pub fn insert(&mut self, k: u64, v: [u8; VAL]) -> bool {
        let path = self.path_to_leaf(k);
        let leaf = *path.last().unwrap();
        let pos = match page::find(&self.pages[leaf as usize][..], k) {
            Ok(_) => return false,
            Err(pos) => pos,
        };
        if page::has_room(&self.pages[leaf as usize][..]) {
            page::insert_at(&mut self.pages[leaf as usize][..], pos, k, &v);
            self.after_change(
                leaf,
                Rec::Insert {
                    page: leaf,
                    key: k,
                    val: v,
                },
            );
            return true;
        }
        // Split, bottom up. Every page touched is logged as an image.
        let mut touched: Vec<u32> = Vec::new();
        let mut carry: Option<(u64, u32)> = None;
        let mut new_root = None;
        for (depth, &pid) in path.iter().enumerate().rev() {
            let (ins_key, rest): (u64, Vec<u8>) = match carry {
                None if depth == path.len() - 1 => (k, v.to_vec()),
                None => break,
                Some((sk, child)) => (sk, child.to_le_bytes().to_vec()),
            };
            let pg = &mut self.pages[pid as usize];
            if page::has_room(&pg[..]) {
                let pos = page::find(&pg[..], ins_key).unwrap_err();
                page::insert_at(&mut pg[..], pos, ins_key, &rest);
                touched.push(pid);
                carry = None;
                break;
            }
            let mut recs = page::records(&pg[..]);
            let pos = recs.partition_point(|(x, _)| *x < ins_key);
            recs.insert(pos, (ins_key, rest));
            let mid = recs.len() / 2;
            let kind = page::kind(&pg[..]);
            let right_id = self.pages.len() as u32;
            let mut right = page::new_page(right_id, kind);
            let sep;
            if kind == LEAF {
                sep = recs[mid].0;
                page::rebuild(&mut right[..], &recs[mid..]);
            } else {
                // The middle key moves up; its child becomes right's leftmost.
                sep = recs[mid].0;
                let c = u32::from_le_bytes(recs[mid].1[..4].try_into().unwrap());
                page::set_leftmost(&mut right[..], c);
                page::rebuild(&mut right[..], &recs[mid + 1..]);
            }
            page::rebuild(&mut self.pages[pid as usize][..], &recs[..mid]);
            self.pages.push(right);
            touched.push(pid);
            touched.push(right_id);
            carry = Some((sep, right_id));
            if depth == 0 {
                let root_id = self.pages.len() as u32;
                let mut r = page::new_page(root_id, INTERNAL);
                page::set_leftmost(&mut r[..], pid);
                page::insert_at(&mut r[..], 0, sep, &right_id.to_le_bytes());
                self.pages.push(r);
                touched.push(root_id);
                new_root = Some(root_id);
            }
        }
        let _ = carry;
        for pid in touched {
            self.image(pid);
        }
        if let Some(r) = new_root {
            self.root = r;
            self.log(Rec::NewRoot { page: r });
        }
        true
    }

    fn write_page(
        &mut self,
        pid: u32,
        crash: Option<&CrashPlan>,
        counted: bool,
    ) -> Result<(), Crashed> {
        if self.disk.is_none() {
            // Bytes-only runs keep no disk; nothing to write.
            return Ok(());
        }
        let mut img = self.pages[pid as usize].clone();
        page::seal(&mut img[..]);
        self.pages[pid as usize][..4].copy_from_slice(&img[..4]);
        let disk = self.disk.as_mut().expect("durable");
        while disk.len() <= pid as usize {
            disk.push(Box::new([0u8; PAGE]));
        }
        if counted {
            let idx = self.writes;
            self.writes += 1;
            if let Some(c) = crash {
                if c.at_write == idx {
                    let units = if self.mode.atomic() { 1 } else { PAGE / c.unit };
                    let usize_ = PAGE / units;
                    let mut x = c.bits;
                    for u in 0..units {
                        x = x
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        let draw = ((x >> 33) & 0xFFFF) as f64 / 65536.0;
                        if draw < c.persist_p {
                            disk[pid as usize][u * usize_..(u + 1) * usize_]
                                .copy_from_slice(&img[u * usize_..(u + 1) * usize_]);
                        }
                    }
                    return Err(Crashed);
                }
            }
        }
        disk[pid as usize].copy_from_slice(&img[..]);
        Ok(())
    }

    /// Checkpoint: log the redo pointer, flush dirty pages, then write the
    /// control file.
    pub fn checkpoint(&mut self, crash: Option<&CrashPlan>) -> Result<(), Crashed> {
        let redo = self.next_lsn;
        self.log(Rec::Checkpoint { redo });
        self.fpi_done.clear();
        let dirty: Vec<u32> = std::mem::take(&mut self.dirty).into_iter().collect();
        for pid in dirty {
            self.write_page(pid, crash, true)?;
        }
        self.control = Control {
            redo,
            root: self.root,
        };
        Ok(())
    }

    /// Write every page once (initial durable image). Not counted as a
    /// crashable write.
    pub fn initial_flush(&mut self) {
        for pid in 0..self.pages.len() as u32 {
            self.write_page(pid, None, false).expect("uncounted");
        }
        self.dirty.clear();
        self.control = Control {
            redo: self.next_lsn,
            root: self.root,
        };
    }

    /// Recover from disk + control + WAL. Returns the recovered engine and
    /// the number of pages that are torn (bad CRC) or could not be redone.
    pub fn recover(&self) -> (Engine, RecoveryReport) {
        let disk = self.disk.as_ref().expect("durable engine");
        let wal = self.wal.as_ref().expect("durable engine");
        let mut pages: Vec<Page> = disk.clone();
        let mut root = self.control.root;
        let mut broken: BTreeSet<u32> = BTreeSet::new();
        let mut checked: HashSet<u32> = HashSet::new();
        let mut redone: HashSet<u32> = HashSet::new();
        let mut applied = 0u64;
        for (lsn, r) in wal.iter().filter(|(l, _)| *l >= self.control.redo) {
            let lsn = *lsn;
            let pid = match r {
                Rec::Update { page, .. } | Rec::Insert { page, .. } | Rec::Image { page, .. } => {
                    *page
                }
                Rec::NewRoot { page } => {
                    root = *page;
                    continue;
                }
                Rec::Checkpoint { .. } => continue,
            };
            while pages.len() <= pid as usize {
                pages.push(Box::new([0u8; PAGE]));
            }
            if broken.contains(&pid) {
                continue;
            }
            if let Rec::Image { data, .. } = r {
                pages[pid as usize] = data.clone();
                page::set_lsn(&mut pages[pid as usize][..], lsn);
                checked.insert(pid);
                redone.insert(pid);
                applied += 1;
                continue;
            }
            let p = &mut pages[pid as usize];
            if !checked.contains(&pid) {
                checked.insert(pid);
                if !page::verify(&p[..]) || !page::sane(&p[..]) || page::kind(&p[..]) != LEAF {
                    broken.insert(pid);
                    continue;
                }
            }
            if page::lsn(&p[..]) >= lsn {
                continue;
            }
            let ok = match r {
                Rec::Update { key, val, .. } => match page::find(&p[..], *key) {
                    Ok(i) => {
                        page::set_value(&mut p[..], i, val);
                        true
                    }
                    Err(_) => false,
                },
                Rec::Insert { key, val, .. } => match page::find(&p[..], *key) {
                    Err(pos) if page::has_room(&p[..]) => {
                        page::insert_at(&mut p[..], pos, *key, val);
                        true
                    }
                    _ => false,
                },
                _ => unreachable!(),
            };
            if !ok {
                broken.insert(pid);
                continue;
            }
            page::set_lsn(&mut p[..], lsn);
            redone.insert(pid);
            applied += 1;
        }
        for pid in &redone {
            if !broken.contains(pid) {
                page::seal(&mut pages[*pid as usize][..]);
            }
        }
        let mut torn = BTreeSet::new();
        for (i, p) in pages.iter().enumerate() {
            if broken.contains(&(i as u32)) || !page::verify(&p[..]) {
                torn.insert(i as u32);
            }
        }
        let e = Engine {
            mode: self.mode,
            pages,
            root,
            dirty: BTreeSet::new(),
            fpi_done: HashSet::new(),
            next_lsn: self.next_lsn,
            wal_bytes: 0,
            wal: None,
            hole_elimination: self.hole_elimination,
            disk: None,
            control: self.control,
            writes: 0,
            fpis: 0,
        };
        (
            e,
            RecoveryReport {
                torn_pages: torn.len() as u64,
                records_applied: applied,
            },
        )
    }

    /// All key/value pairs reachable from the root, or None if the tree is
    /// structurally broken.
    pub fn scan(&self) -> Option<BTreeMap<u64, [u8; VAL]>> {
        let mut out = BTreeMap::new();
        let mut stack = vec![self.root];
        let mut seen = HashSet::new();
        while let Some(pid) = stack.pop() {
            if !seen.insert(pid) {
                return None;
            }
            let p = self.pages.get(pid as usize)?;
            if !page::verify(&p[..]) || !page::sane(&p[..]) {
                return None;
            }
            match page::kind(&p[..]) {
                LEAF => {
                    for i in 0..page::nslots(&p[..]) {
                        out.insert(
                            page::key(&p[..], i),
                            page::value(&p[..], i).try_into().unwrap(),
                        );
                    }
                }
                INTERNAL => {
                    stack.push(page::leftmost(&p[..]));
                    for i in 0..page::nslots(&p[..]) {
                        stack.push(page::child(&p[..], i));
                    }
                }
                _ => return None,
            }
        }
        Some(out)
    }

    /// Seal every in-memory page (so `scan` can verify the live tree).
    pub fn seal_all(&mut self) {
        for p in self.pages.iter_mut() {
            page::seal(&mut p[..]);
        }
    }
}

/// What recovery found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Pages left torn or not redoable.
    pub torn_pages: u64,
    /// Records applied.
    pub records_applied: u64,
}
