//! Kymo viewer binary. The GUI arrives in phase 2; for now it opens a file and prints a
//! summary, which is handy for checking real recordings.

use std::path::Path;

use anyhow::{Context, Result, bail};
use kymo_core::Recording;

const USAGE: &str = "Usage: kymo <file.edf|file.bdf>";

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next()) {
        (Some(arg), None) if arg == "-h" || arg == "--help" => {
            println!("kymo {}\n{USAGE}", kymo_core::version());
            Ok(())
        }
        (Some(path), None) => summarize(Path::new(&path)),
        _ => bail!("{USAGE}"),
    }
}

fn summarize(path: &Path) -> Result<()> {
    let opened = kymo_core::open(path)?;
    let (recording, annotations) =
        Recording::new(opened).with_context(|| format!("invalid recording {}", path.display()))?;
    let start = recording
        .start_time()
        .map_or_else(|| "unknown".to_owned(), |t| t.to_string());
    println!("{}", path.display());
    println!(
        "  {}, start {start}, {:.3} s, {} channels, {} annotations",
        recording.format(),
        recording.duration(),
        recording.channels().len(),
        annotations.len(),
    );
    for (g, grid) in recording.grids().iter().enumerate() {
        let names: Vec<&str> = grid
            .channels()
            .iter()
            .map(|&c| recording.channel(c).name.as_str())
            .collect();
        println!(
            "  grid {g}: {} Hz, {} samples, {} segment(s), {:?}: {}",
            grid.rate(),
            grid.len(),
            grid.segments().len(),
            grid.sample_type(),
            names.join(", ")
        );
    }
    for warning in recording.warnings() {
        println!("  warning: {warning}");
    }
    println!("(the viewer is not implemented yet)");
    Ok(())
}
