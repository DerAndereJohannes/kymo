//! Command-line front end of the synthetic EDF/BDF, BrainVision, EEGLAB, GDF, FIFF and XDF
//! generator.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use kymo_testgen::{PRESETS, Spec};

const HELP: &str = "\
kymo-testgen: generate synthetic EDF/BDF, BrainVision, EEGLAB, GDF, FIFF and XDF files for tests
and benchmarks

Usage: kymo-testgen <PRESET> <OUTPUT> [OPTIONS]
       kymo-testgen --list

Options:
      --duration <SECONDS>  Change the duration (rounded up to whole records)
      --seed <N>            Change the seed of noise and spike positions
  -l, --list                List the presets
  -h, --help                Print this help

BrainVision presets write OUTPUT (the .vhdr) and its .eeg and .vmrk beside it.
EEGLAB presets write OUTPUT (the .set), and its .fdt beside it for eeglab-fdt.
fif-split writes OUTPUT (the first part) and its other parts beside it (OUTPUT-1.fif, ...).";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut positional = Vec::new();
    let (mut duration, mut seed) = (None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(());
            }
            "-l" | "--list" => {
                let width = PRESETS.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
                for (name, description) in PRESETS {
                    println!("{name:<width$} {description}");
                }
                return Ok(());
            }
            "--duration" => {
                let value = args.next().context("--duration needs a value")?;
                let seconds: f64 = value
                    .parse()
                    .with_context(|| format!("invalid duration {value:?}"))?;
                if !(seconds.is_finite() && seconds > 0.0) {
                    bail!("duration must be > 0");
                }
                duration = Some(seconds);
            }
            "--seed" => {
                let value = args.next().context("--seed needs a value")?;
                seed = Some(
                    value
                        .parse::<u64>()
                        .with_context(|| format!("invalid seed {value:?}"))?,
                );
            }
            other if other.starts_with('-') => bail!("unknown option {other}; see --help"),
            _ => positional.push(arg),
        }
    }
    let [preset, output] = positional.as_slice() else {
        bail!("expected a preset and an output file; see --help");
    };

    let mut spec =
        Spec::preset(preset).with_context(|| format!("unknown preset {preset:?}; see --list"))?;
    if let Some(seconds) = duration {
        set_duration(&mut spec, seconds);
    }
    if let Some(seed) = seed {
        spec.seed = seed;
    }

    let output = PathBuf::from(output);
    let size = spec.file_size()?;
    let started = Instant::now();
    kymo_testgen::write(&spec, &output)
        .with_context(|| format!("cannot write {}", output.display()))?;
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "wrote {}: {}, {} channels, {} records of {} s, {:.1} MB in {:.2} s ({:.0} MB/s)",
        output.display(),
        spec.format.name(),
        spec.channels.len(),
        spec.n_records,
        spec.record_duration,
        size as f64 / 1e6,
        elapsed,
        size as f64 / 1e6 / elapsed,
    );
    Ok(())
}

/// Changes the number of records; drops gaps and annotations past the new end.
fn set_duration(spec: &mut Spec, seconds: f64) {
    spec.n_records = ((seconds / spec.record_duration).ceil() as u64).max(1);
    let n = spec.n_records;
    spec.gaps.retain(|g| g.before_record < n);
    let end = spec.record_onset(n - 1) + spec.record_duration;
    spec.annotations.retain(|a| a.onset < end);
}
