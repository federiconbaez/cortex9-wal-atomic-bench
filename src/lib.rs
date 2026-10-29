//! wal-atomic-bench: how many WAL bytes full page images (FPI) cost in a new
//! B+tree, compared with relying on atomic page writes, plus a crash harness
//! that checks each protection against torn pages.
//!
//! - [`page`]: slotted 16 KiB pages with a page LSN and a CRC32C.
//! - [`engine`]: B+tree, WAL (physiological records, FPIs, split images),
//!   checkpoints, page writes that can tear, and ARIES-style redo.
//! - [`bench`]: WAL bytes per transaction.
//! - [`crash`]: crash harness.
//! - [`detect`]: `RWF_ATOMIC` support detection via `statx`.

pub mod bench;
pub mod crash;
pub mod crc;
pub mod detect;
pub mod engine;
pub mod page;
pub mod prereg;
pub mod rng;
