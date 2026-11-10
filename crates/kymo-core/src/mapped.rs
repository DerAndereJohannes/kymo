//! Source files mapped read-only, and reading ahead into the operating system's cache.

use std::fs::File;
use std::ops::Range;

use memmap2::Mmap;

/// Maps `file` read-only.
#[allow(unsafe_code)]
pub(crate) fn map_read_only(file: &File) -> std::io::Result<Mmap> {
    // SAFETY: the mapping is read-only and kymo never writes to source files. Memory mapping
    // is only unsound if another process modifies or truncates the file while it is mapped;
    // like every viewer that maps its input, we accept that (design: source files are
    // memory-mapped read-only and never modified). All reads go through bounds-checked slices.
    unsafe { Mmap::map(file) }
}

/// Bytes read per call when prefetching.
const PREFETCH_PIECE: usize = 8 << 20;

/// Reads the byte ranges of `file` (clamped to its first `len` bytes) into the operating
/// system's cache. Plain reads fill the page cache much faster than page faults on a mapping
/// (on macOS about 6 against 2 GB/s, even with madvise). Errors only cost speed, so they
/// end the prefetch quietly.
pub(crate) fn prefetch(file: &File, len: u64, ranges: impl IntoIterator<Item = Range<u64>>) {
    let mut buf = Vec::new();
    for range in ranges {
        let end = range.end.min(len);
        let mut at = range.start;
        if at >= end {
            continue;
        }
        buf.resize(((end - at) as usize).min(PREFETCH_PIECE), 0);
        while at < end {
            let piece = &mut buf[..(end - at).min(PREFETCH_PIECE as u64) as usize];
            match read_at(file, piece, at) {
                Ok(0) | Err(_) => return,
                Ok(read) => at += read as u64,
            }
        }
    }
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

#[cfg(not(any(unix, windows)))]
fn read_at(_: &File, _: &mut [u8], _: u64) -> std::io::Result<usize> {
    Ok(0)
}
