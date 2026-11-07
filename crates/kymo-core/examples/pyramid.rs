//! Builds the pyramid of every grid of a recording and reports the speed.
//!
//! ```text
//! cargo run --release -p kymo-core --example pyramid -- FILE [--threads N] [--sidecar DIR]
//! ```
//!
//! Without `--sidecar` the pyramid is kept in memory. With it, it is written to a sidecar
//! file in DIR, as the viewer does; an existing complete sidecar is reused, not rebuilt.
//! To time a build from disk rather than from the page cache, evict the file first (macOS:
//! `sudo purge`; Linux: `sync; echo 3 | sudo tee /proc/sys/vm/drop_caches`).

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use kymo_core::pyramid::{PyramidBuilder, Sidecar};
use kymo_core::{GridId, Recording};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut file, mut threads, mut sidecar) = (None, None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--threads" => {
                let n = args.next().context("--threads needs a value")?;
                threads = Some(n.parse::<usize>().context("invalid --threads")?);
            }
            "--sidecar" => {
                sidecar = Some(PathBuf::from(
                    args.next().context("--sidecar needs a directory")?,
                ))
            }
            _ if file.is_none() && !arg.starts_with('-') => file = Some(PathBuf::from(arg)),
            _ => bail!("usage: pyramid FILE [--threads N] [--sidecar DIR]"),
        }
    }
    let file = file.context("usage: pyramid FILE [--threads N] [--sidecar DIR]")?;

    let start = Instant::now();
    let (rec, _) = Recording::new(kymo_core::open(&file)?)?;
    println!("opened in {:.3} s", start.elapsed().as_secs_f64());
    let bytes = std::fs::metadata(&file)?.len() as f64;

    let start = Instant::now();
    let built = match &sidecar {
        Some(dir) => in_sidecar(&rec, &file, dir, threads)?,
        None => {
            for g in 0..rec.grids().len() as u32 {
                let builder = PyramidBuilder::new(&rec, GridId(g))?;
                with_threads(builder, threads).finish()?;
            }
            true
        }
    };
    let seconds = start.elapsed().as_secs_f64();
    if built {
        println!(
            "built {:.0} MB in {seconds:.2} s: {:.0} MB/s",
            bytes / 1e6,
            bytes / 1e6 / seconds
        );
    } else {
        println!("reused the complete sidecar, opened in {seconds:.3} s");
    }
    Ok(())
}

/// Builds the sidecar in `dir`; returns false if it was complete already.
fn in_sidecar(rec: &Recording, file: &Path, dir: &Path, threads: Option<usize>) -> Result<bool> {
    let sidecar = Sidecar::open_or_create(dir, file, rec)?;
    if sidecar.is_complete() {
        return Ok(false);
    }
    for g in 0..rec.grids().len() as u32 {
        let builder = sidecar
            .builder(rec, GridId(g))
            .context("another build holds this sidecar")?;
        with_threads(builder, threads).finish()?;
    }
    sidecar.finish()?;
    println!("sidecar: {}", sidecar.path().display());
    Ok(true)
}

fn with_threads(builder: PyramidBuilder<'_>, threads: Option<usize>) -> PyramidBuilder<'_> {
    match threads {
        Some(n) => builder.with_threads(n),
        None => builder,
    }
}
