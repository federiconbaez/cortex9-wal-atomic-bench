//! Slotted 16 KiB page.
//!
//! ```text
//! 0..4    crc32c over 4..PAGE
//! 4..8    page id
//! 8..16   page LSN
//! 16      kind (1 leaf, 2 internal)
//! 18..20  slot count n
//! 20..22  heap start (records grow down from the end of the page)
//! 22..26  leftmost child (internal pages)
//! 32..    slot array, u16 offsets, sorted by key
//! ...     hole (free space)
//! heap..  records: leaf = key(8) + value(100), internal = key(8) + child(4)
//! ```

use crate::crc::crc32c;

/// Page size.
pub const PAGE: usize = 16384;
/// Header size.
pub const HDR: usize = 32;
/// Value size.
pub const VAL: usize = 100;
/// Leaf record size.
pub const LEAF_REC: usize = 8 + VAL;
/// Internal record size.
pub const INT_REC: usize = 8 + 4;
/// Leaf page kind.
pub const LEAF: u8 = 1;
/// Internal page kind.
pub const INTERNAL: u8 = 2;

/// A page buffer.
pub type Page = Box<[u8; PAGE]>;

fn r16(p: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([p[o], p[o + 1]])
}
fn w16(p: &mut [u8], o: usize, v: u16) {
    p[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn r32(p: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(p[o..o + 4].try_into().unwrap())
}
fn w32(p: &mut [u8], o: usize, v: u32) {
    p[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn r64(p: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(p[o..o + 8].try_into().unwrap())
}
fn w64(p: &mut [u8], o: usize, v: u64) {
    p[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

/// New empty page.
pub fn new_page(id: u32, kind: u8) -> Page {
    let mut p: Page = Box::new([0u8; PAGE]);
    w32(&mut p[..], 4, id);
    p[16] = kind;
    w16(&mut p[..], 18, 0);
    // Heap start 0 means PAGE (empty heap); PAGE does not fit in u16.
    w16(&mut p[..], 20, 0);
    p
}

/// Page id.
pub fn id(p: &[u8]) -> u32 {
    r32(p, 4)
}
/// Page LSN.
pub fn lsn(p: &[u8]) -> u64 {
    r64(p, 8)
}
/// Set page LSN.
pub fn set_lsn(p: &mut [u8], v: u64) {
    w64(p, 8, v)
}
/// Kind.
pub fn kind(p: &[u8]) -> u8 {
    p[16]
}
/// Slot count.
pub fn nslots(p: &[u8]) -> usize {
    r16(p, 18) as usize
}
/// Heap start offset.
pub fn heap(p: &[u8]) -> usize {
    match r16(p, 20) {
        0 => PAGE,
        h => h as usize,
    }
}
fn set_heap(p: &mut [u8], h: usize) {
    w16(p, 20, if h == PAGE { 0 } else { h as u16 });
}
/// Leftmost child of an internal page.
pub fn leftmost(p: &[u8]) -> u32 {
    r32(p, 22)
}
/// Set leftmost child.
pub fn set_leftmost(p: &mut [u8], c: u32) {
    w32(p, 22, c)
}
fn slot(p: &[u8], i: usize) -> usize {
    r16(p, HDR + 2 * i) as usize
}
fn rec_size(p: &[u8]) -> usize {
    if kind(p) == LEAF {
        LEAF_REC
    } else {
        INT_REC
    }
}

/// Key at slot `i`.
pub fn key(p: &[u8], i: usize) -> u64 {
    r64(p, slot(p, i))
}
/// Value at slot `i` (leaf).
pub fn value(p: &[u8], i: usize) -> &[u8] {
    let o = slot(p, i) + 8;
    &p[o..o + VAL]
}
/// Child at slot `i` (internal).
pub fn child(p: &[u8], i: usize) -> u32 {
    r32(p, slot(p, i) + 8)
}

/// Bytes used outside the hole: header + slots, and the heap.
pub fn used_ranges(p: &[u8]) -> (usize, usize) {
    (HDR + 2 * nslots(p), heap(p))
}

/// Free bytes in the hole.
pub fn free(p: &[u8]) -> usize {
    let (lo, hi) = used_ranges(p);
    hi.saturating_sub(lo)
}

/// True if one more record fits.
pub fn has_room(p: &[u8]) -> bool {
    free(p) >= 2 + rec_size(p)
}

/// Binary search for `k`: Ok(slot) or Err(insert position).
pub fn find(p: &[u8], k: u64) -> Result<usize, usize> {
    let (mut lo, mut hi) = (0usize, nslots(p));
    while lo < hi {
        let mid = (lo + hi) / 2;
        let km = key(p, mid);
        if km == k {
            return Ok(mid);
        }
        if km < k {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Err(lo)
}

/// Child to follow for `k` in an internal page.
pub fn route(p: &[u8], k: u64) -> u32 {
    match find(p, k) {
        Ok(i) => child(p, i),
        Err(0) => leftmost(p),
        Err(i) => child(p, i - 1),
    }
}

/// Overwrite the value at slot `i`.
pub fn set_value(p: &mut [u8], i: usize, v: &[u8]) {
    let o = slot(p, i) + 8;
    p[o..o + VAL].copy_from_slice(v);
}

/// Insert a record at slot position `pos`. `rest` is the value (leaf) or the
/// child id as 4 bytes (internal). Caller checks `has_room`.
pub fn insert_at(p: &mut [u8], pos: usize, k: u64, rest: &[u8]) {
    let rs = rec_size(p);
    assert_eq!(rest.len(), rs - 8);
    let n = nslots(p);
    assert!(pos <= n && has_room(p));
    let h = heap(p) - rs;
    w64(p, h, k);
    p[h + 8..h + rs].copy_from_slice(rest);
    set_heap(p, h);
    p.copy_within(HDR + 2 * pos..HDR + 2 * n, HDR + 2 * pos + 2);
    w16(p, HDR + 2 * pos, h as u16);
    w16(p, 18, (n + 1) as u16);
}

/// Rebuild `p` with exactly `recs` (sorted), compacting the heap.
pub fn rebuild(p: &mut [u8], recs: &[(u64, Vec<u8>)]) {
    let kd = kind(p);
    let pid = id(p);
    let lm = leftmost(p);
    let l = lsn(p);
    p.fill(0);
    w32(p, 4, pid);
    p[16] = kd;
    set_lsn(p, l);
    set_leftmost(p, lm);
    set_heap(p, PAGE);
    for (i, (k, r)) in recs.iter().enumerate() {
        insert_at(p, i, *k, r);
    }
}

/// All records as (key, rest).
pub fn records(p: &[u8]) -> Vec<(u64, Vec<u8>)> {
    let rs = rec_size(p);
    (0..nslots(p))
        .map(|i| {
            let o = slot(p, i);
            (r64(p, o), p[o + 8..o + rs].to_vec())
        })
        .collect()
}

/// Seal the CRC.
pub fn seal(p: &mut [u8]) {
    let c = crc32c(&p[4..]);
    w32(p, 0, c);
}

/// Verify the CRC.
pub fn verify(p: &[u8]) -> bool {
    r32(p, 0) == crc32c(&p[4..])
}

/// Basic structural sanity (used by redo before touching a page).
pub fn sane(p: &[u8]) -> bool {
    let n = nslots(p);
    let h = heap(p);
    let rs = match kind(p) {
        LEAF => LEAF_REC,
        INTERNAL => INT_REC,
        _ => return false,
    };
    if HDR + 2 * n > h || h > PAGE {
        return false;
    }
    (0..n).all(|i| {
        let o = slot(p, i);
        o >= h && o + rs <= PAGE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_find_update_and_hole() {
        let mut p = new_page(3, LEAF);
        let v = [7u8; VAL];
        let mut n = 0;
        let mut k = 1000u64;
        while has_room(&p[..]) {
            let pos = find(&p[..], k).unwrap_err();
            insert_at(&mut p[..], pos, k, &v);
            k = k.wrapping_mul(6364136223846793005).wrapping_add(1) % 1_000_000;
            n += 1;
        }
        assert_eq!(n, (PAGE - HDR) / (LEAF_REC + 2));
        let ks: Vec<u64> = (0..nslots(&p[..])).map(|i| key(&p[..], i)).collect();
        assert!(ks.windows(2).all(|w| w[0] < w[1]));
        let i = find(&p[..], 1000).unwrap();
        set_value(&mut p[..], i, &[9u8; VAL]);
        assert_eq!(value(&p[..], i), &[9u8; VAL]);
        assert!(free(&p[..]) < 2 + LEAF_REC);
        seal(&mut p[..]);
        assert!(verify(&p[..]) && sane(&p[..]));
        p[5000] ^= 1;
        assert!(!verify(&p[..]));
    }
}
