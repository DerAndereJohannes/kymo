//! The sidecar's memory mapping: the only place that touches its memory.

// Raw pointer access is unavoidable here: the builder writes tiles into the mapping while
// readers on other threads read tiles it has already published.
#![allow(unsafe_code)]

use std::fs::File;
use std::io;

use memmap2::{MmapOptions, MmapRaw};

/// A mapping shared between one writer per region and any number of readers.
pub(super) struct SharedMap {
    raw: MmapRaw,
    writable: bool,
}

impl SharedMap {
    /// Maps `file` read-write. The file must not be changed by anyone else while mapped:
    /// sidecars are built in a file with a unique name that only this process opens.
    pub(super) fn writable(file: &File) -> io::Result<SharedMap> {
        Ok(SharedMap {
            raw: MmapOptions::new().map_raw(file)?,
            writable: true,
        })
    }

    /// Maps `file` read-only. Like the source recording, the file must not be truncated or
    /// rewritten in place while mapped; kymo only ever replaces sidecars by renaming.
    pub(super) fn read_only(file: &File) -> io::Result<SharedMap> {
        Ok(SharedMap {
            raw: MmapOptions::new().map_raw_read_only(file)?,
            writable: false,
        })
    }

    pub(super) fn len(&self) -> usize {
        self.raw.len()
    }

    /// Copies `bytes` to `offset`.
    ///
    /// # Safety
    /// No other thread may read or write `offset..offset + bytes.len()` during the call, and
    /// `bytes` must not point into the mapping.
    ///
    /// # Panics
    /// If the mapping is read-only or the range is outside it.
    pub(super) unsafe fn write(&self, offset: usize, bytes: &[u8]) {
        assert!(self.writable, "write to a read-only sidecar mapping");
        let end = offset.checked_add(bytes.len());
        assert!(
            end.is_some_and(|end| end <= self.len()),
            "write outside the mapping"
        );
        // SAFETY: the range lies inside the mapping (checked above), which is writable and
        // lives as long as `self`. The caller guarantees that no other thread accesses the
        // range meanwhile, so this is not a data race, and that `bytes` lies outside the
        // mapping, so source and destination do not overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.raw.as_mut_ptr().add(offset),
                bytes.len(),
            );
        }
    }

    /// The bytes at `offset..offset + len`.
    ///
    /// # Safety
    /// No thread may write the range while the returned slice is alive.
    ///
    /// # Panics
    /// If the range is outside the mapping.
    pub(super) unsafe fn read(&self, offset: usize, len: usize) -> &[u8] {
        let end = offset.checked_add(len);
        assert!(
            end.is_some_and(|end| end <= self.len()),
            "read outside the mapping"
        );
        // SAFETY: the range lies inside the mapping (checked above), which lives as long as
        // the returned borrow of `self`. The caller guarantees that nothing writes the range
        // while the slice is alive, so the shared borrow is not aliased by a mutation.
        unsafe { std::slice::from_raw_parts(self.raw.as_ptr().add(offset), len) }
    }

    pub(super) fn flush(&self) -> io::Result<()> {
        self.raw.flush()
    }
}
