//! Detect `RWF_ATOMIC` support for a path with `statx(STATX_WRITE_ATOMIC)`.
//! Detection only: nothing here issues atomic writes.

/// What `statx` reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtomicSupport {
    /// The kernel filled the atomic-write fields (`STATX_WRITE_ATOMIC` in
    /// the returned mask).
    pub reported: bool,
    /// Minimum atomic write unit in bytes (0 if unsupported).
    pub unit_min: u32,
    /// Maximum atomic write unit in bytes (0 if unsupported).
    pub unit_max: u32,
    /// Max segments per atomic write.
    pub segments_max: u32,
}

/// `STATX_WRITE_ATOMIC` mask bit (Linux 6.11).
pub const STATX_WRITE_ATOMIC: u32 = 0x0001_0000;

/// Query `path`. `Err` carries errno or "not linux".
#[cfg(target_os = "linux")]
pub fn query(path: &std::path::Path) -> Result<AtomicSupport, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // struct statx is 256 bytes; read the fields by offset so this builds
    // against libc versions that predate the atomic-write fields.
    let mut buf = [0u64; 32];
    // SAFETY: valid NUL-terminated path, 256-byte writable buffer, as the
    // statx(2) ABI requires.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_statx,
            libc::AT_FDCWD,
            c.as_ptr(),
            0,
            STATX_WRITE_ATOMIC | libc::STATX_BASIC_STATS,
            buf.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_ne_bytes()).collect();
    let u32_at = |o: usize| u32::from_ne_bytes(bytes[o..o + 4].try_into().unwrap());
    let mask = u32_at(0);
    let reported = mask & STATX_WRITE_ATOMIC != 0;
    // Offsets from include/uapi/linux/stat.h (6.11): stx_atomic_write_unit_min
    // at 168, _max at 172, _segments_max at 176.
    Ok(AtomicSupport {
        reported,
        unit_min: if reported { u32_at(168) } else { 0 },
        unit_max: if reported { u32_at(172) } else { 0 },
        segments_max: if reported { u32_at(176) } else { 0 },
    })
}

/// Not Linux: no `statx`.
#[cfg(not(target_os = "linux"))]
pub fn query(_path: &std::path::Path) -> Result<AtomicSupport, String> {
    Err("not linux".into())
}
