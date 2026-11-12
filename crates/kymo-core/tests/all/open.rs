//! Opening files: formats recognized from their first bytes, and header files opened in
//! place of the data or marker files beside them.

use std::path::{Path, PathBuf};

use crate::common::{TempDir, TempFile};
use kymo_core::OpenError;

const BRAINVISION: &[u8] = b"Brain Vision Data Exchange Header File Version 1.0\r\n; comment\r\n";
const MAT: &[u8] = b"MATLAB 5.0 MAT-file, Platform: GLNXA64, Created on: Mon Jan  1 00:00:00 2024";

/// The reason `path` could not be opened, and the file it names: unsupported, or a
/// BrainVision header, MAT-file, GDF header, FIFF file id or XDF magic the reader refused
/// (these are only first lines).
fn unsupported(path: &Path) -> (PathBuf, String) {
    match kymo_core::open(path) {
        Err(OpenError::Unsupported { path, reason }) => (path, reason),
        Err(OpenError::BrainVision { path, source }) => (path, format!("BrainVision: {source}")),
        Err(OpenError::Eeglab { path, source }) => (path, format!("EEGLAB: {source}")),
        Err(OpenError::Gdf { path, source }) => (path, format!("GDF: {source}")),
        Err(OpenError::Fiff { path, source }) => (path, format!("FIFF: {source}")),
        Err(OpenError::Xdf { path, source }) => (path, format!("XDF: {source}")),
        other => panic!("{}: expected a refusal, got {other:?}", path.display()),
    }
}

fn write(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

const UNKNOWN: &str = "not a supported file format";

#[test]
fn formats_are_recognized_by_their_first_bytes() {
    let fiff = [0, 0, 0, 100, 0, 0, 0, 31, 0, 0, 0, 20, 0, 0, 0, 0];
    let cases: &[(&[u8], &str)] = &[
        (b"XDF:\x01\x2a\x00\x00", "XDF: "),
        (b"GDF 1.25", "GDF: "),
        (b"GDF 2.10", "GDF: "),
        (b"GDF 3.00", "only GDF 1 and 2"),
        (&fiff, "FIFF: "),
        (b"\x1f\x8b\x08\x00", "compressed"),
        (BRAINVISION, "BrainVision: "),
        (
            b"\xef\xbb\xbfBrainVision Data Exchange Header File Version 2.0\n",
            "BrainVision: ",
        ),
        (
            b"Brain Vision Core Data Exchange Header File Version 1.0\r\n",
            "BrainVision: ",
        ),
        (
            b"Brain Vision V-Amp Data Header File Version 1.0\r\n",
            "BrainVision: ",
        ),
        (MAT, "EEGLAB: "),
        (b"MATLAB 7.3 MAT-file, Platform: GLNXA64", "HDF5"),
        // Not quite any of them.
        (b"BrainVision rocks\nHeader File", UNKNOWN),
        (b"XDF", UNKNOWN),
        (&fiff[..7], UNKNOWN),
        (b"MATLAB 4", UNKNOWN),
        (b"\x89PNG\r\n\x1a\n", UNKNOWN),
        (b"", UNKNOWN),
    ];
    let dir = TempDir::new();
    for (k, &(bytes, expected)) in cases.iter().enumerate() {
        // The extension never matters, so none of these has a telling one.
        let path = write(&dir, &format!("file{k}.dat"), bytes);
        let (named, reason) = unsupported(&path);
        assert_eq!(named, path);
        assert!(reason.contains(expected), "{bytes:?}: {reason}");
    }
}

#[test]
fn edf_and_bdf_open_whatever_their_extension() {
    for (preset, format) in [("bdf+c-small", "BDF+C"), ("edf+d-mixed", "EDF+D")] {
        let file = TempFile::preset(preset);
        let dir = TempDir::new();
        let renamed = write(&dir, "recording.xyz", &std::fs::read(file.path()).unwrap());
        assert_eq!(kymo_core::open(&renamed).unwrap().header.format, format);
    }
}

#[test]
fn data_and_marker_files_open_their_header_file() {
    let dir = TempDir::new();
    let vhdr = write(&dir, "rec.vhdr", BRAINVISION);
    let set = write(&dir, "set.set", MAT);
    for (name, header) in [
        ("rec.eeg", &vhdr),
        ("rec.vmrk", &vhdr),
        ("rec.EEG", &vhdr),
        ("set.fdt", &set),
        ("set.FDT", &set),
    ] {
        // Samples and markers have no telling first bytes.
        let path = write(&dir, name, b"\x01\x02\x03\x04 data");
        let (named, reason) = unsupported(&path);
        assert_eq!(&named, header, "{name}");
        assert!(!reason.contains(UNKNOWN), "{name}: {reason}");
    }
}

#[test]
fn header_files_are_found_in_either_case() {
    let dir = TempDir::new();
    let vhdr = write(&dir, "REC.VHDR", BRAINVISION);
    let eeg = write(&dir, "REC.EEG", b"\x01\x02");
    let (named, reason) = unsupported(&eeg);
    assert!(reason.contains("BrainVision"), "{reason}");
    // A case-insensitive file system finds "REC.vhdr" first, which is the same file.
    assert_eq!(
        std::fs::canonicalize(named).unwrap(),
        std::fs::canonicalize(vhdr).unwrap()
    );
}

#[test]
fn a_data_file_without_a_recognized_header_file_is_not_supported() {
    let dir = TempDir::new();
    // No header file beside it.
    let lone = write(&dir, "lone.eeg", b"\x01\x02");
    let (named, reason) = unsupported(&lone);
    assert_eq!(named, lone);
    assert!(reason.contains(UNKNOWN), "{reason}");
    // A header file that isn't one: the data file is reported, not it.
    write(&dir, "odd.vhdr", b"not a header");
    let odd = write(&dir, "odd.eeg", b"\x01\x02");
    let (named, reason) = unsupported(&odd);
    assert_eq!(named, odd);
    assert!(reason.contains(UNKNOWN), "{reason}");
    // Other extensions never look for a header file.
    write(&dir, "other.vhdr", BRAINVISION);
    let other = write(&dir, "other.dat", b"\x01\x02");
    assert_eq!(unsupported(&other).0, other);
}

#[test]
fn content_wins_over_a_header_file_beside_it() {
    // A recognized file named like a BrainVision data file still opens as what it is.
    let file = TempFile::preset("bdf+c-small");
    let dir = TempDir::new();
    write(&dir, "rec.vhdr", BRAINVISION);
    let eeg = write(&dir, "rec.eeg", &std::fs::read(file.path()).unwrap());
    assert_eq!(kymo_core::open(&eeg).unwrap().header.format, "BDF+C");
}

#[test]
fn missing_files_are_io_errors() {
    let dir = TempDir::new();
    let missing = dir.path().join("missing.edf");
    match kymo_core::open(&missing) {
        Err(OpenError::Io { path, .. }) => assert_eq!(path, missing),
        other => panic!("expected Io, got {other:?}"),
    }
}
