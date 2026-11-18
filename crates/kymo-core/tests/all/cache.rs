//! The disk cache: its size, and clearing it without touching builds in progress, kept
//! sidecars or other files.

use crate::common::{TempDir, TempFile};
use kymo_core::cache::{self, Cleared, Usage};
use kymo_core::pyramid::Sidecar;
use kymo_core::{GridId, Recording};
use kymo_testgen::Spec;

const HASH: &str = "0d5284d4f6dad0bd4380c50b1337de3dbb3982bdc8522b48e7e6ef04bebbaafd";

fn generated(preset: &str) -> (TempFile, Recording) {
    let file = TempFile::written(&Spec::preset(preset).unwrap());
    let rec = Recording::new(kymo_core::open(file.path()).unwrap())
        .unwrap()
        .0;
    (file, rec)
}

fn write(dir: &TempDir, name: &str, len: usize) {
    std::fs::write(dir.path().join(name), vec![0u8; len]).unwrap();
}

#[test]
fn a_missing_directory_holds_nothing() {
    let dir = TempDir::new();
    let missing = dir.path().join("missing");
    assert_eq!(cache::usage(&missing).unwrap(), Usage::default());
    assert_eq!(cache::clear(&missing, &[]).unwrap(), Cleared::default());
}

#[test]
fn only_kymos_files_count_and_go() {
    let dir = TempDir::new();
    write(&dir, &format!("{HASH}.kymo-pyramid"), 100);
    // Left behind by a build that never finished: no one holds its lock.
    write(&dir, &format!("{HASH}.4242-0.tmp"), 20);
    write(&dir, "times.4242-1.tmp", 3);
    write(&dir, "notes.txt", 1000);
    write(&dir, "abc.kymo-pyramid", 1000);
    std::fs::create_dir(dir.path().join(format!("{}.kymo-pyramid", &HASH[..63]))).unwrap();
    assert_eq!(
        cache::usage(dir.path()).unwrap(),
        Usage {
            bytes: 123,
            files: 3
        }
    );
    let cleared = cache::clear(dir.path(), &[]).unwrap();
    assert_eq!(
        cleared,
        Cleared {
            removed: Usage {
                bytes: 123,
                files: 3
            },
            kept: 0
        }
    );
    assert_eq!(cache::usage(dir.path()).unwrap(), Usage::default());
    // The other files and the directory.
    assert_eq!(dir.files().len(), 3, "{:?}", dir.files());
    assert!(dir.path().join("notes.txt").exists());
    assert!(dir.path().join("abc.kymo-pyramid").exists());
}

#[test]
fn kept_sidecars_stay() {
    let (_a_file, a) = generated("bdf+c-small");
    let (_b_file, b) = generated("edf+d-mixed");
    let dir = TempDir::new();
    let kept = Sidecar::open_or_create(dir.path(), &a).unwrap();
    assert!(kept.build(&a).unwrap());
    let other = Sidecar::open_or_create(dir.path(), &b).unwrap();
    assert!(other.build(&b).unwrap());
    drop(other);
    assert_eq!(cache::usage(dir.path()).unwrap().files, 2);

    let cleared = cache::clear(dir.path(), &[kept.path()]).unwrap();
    assert_eq!(cleared.removed.files, 1);
    assert_eq!(cleared.kept, 1);
    assert!(kept.path().exists());
    // The kept one opens complete again, the other is rebuilt.
    assert!(
        Sidecar::open_or_create(dir.path(), &a)
            .unwrap()
            .is_complete()
    );
    assert!(
        !Sidecar::open_or_create(dir.path(), &b)
            .unwrap()
            .is_complete()
    );
}

#[test]
fn a_build_in_progress_survives_and_finishes() {
    let (_file, rec) = generated("bdf+c-small");
    let dir = TempDir::new();
    let sidecar = Sidecar::open_or_create(dir.path(), &rec).unwrap();
    let mut builder = sidecar
        .builder(&rec, GridId(0))
        .unwrap()
        .with_chunk_len(1000);
    builder.step().unwrap();
    let building = sidecar.path();

    let cleared = cache::clear(dir.path(), &[]).unwrap();
    assert_eq!(cleared.removed, Usage::default());
    assert_eq!(cleared.kept, 1);
    assert!(building.exists());

    builder.finish().unwrap();
    assert!(sidecar.finish().unwrap());
    assert!(sidecar.is_complete());
    // Its lock is gone with the build.
    let file = std::fs::File::open(sidecar.path()).unwrap();
    assert!(file.try_lock().is_ok());
    drop(file);
    drop(sidecar);
    assert!(
        Sidecar::open_or_create(dir.path(), &rec)
            .unwrap()
            .is_complete()
    );

    // Complete, it is a sidecar like any other.
    assert_eq!(cache::clear(dir.path(), &[]).unwrap().removed.files, 1);
    assert!(dir.files().is_empty(), "{:?}", dir.files());
}
