//! Saves and loads annotation files as the viewer does, for comparison with MNE
//! (`scripts/compare_mne.py --annotations`).
//!
//! ```text
//! cargo run --release -p kymo-core --example annotations -- save FILE OUT.tsv
//! cargo run --release -p kymo-core --example annotations -- load FILE IN.tsv|IN.csv
//! ```
//!
//! `save` writes the recording's own annotations as an events table; `load` reads a table or
//! MNE's CSV for the recording and prints JSON: the annotations (onset in kymo's time,
//! duration, description, channel names) and how many were skipped.

use std::fmt::Write as _;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use kymo_core::Recording;
use kymo_core::annotation_file::{Context, Format, read, write_table};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [command, file, other] = args.as_slice() else {
        bail!("usage: annotations save|load FILE ANNOTATIONS");
    };
    let (recording, annotations) = Recording::new(kymo_core::open(Path::new(file))?)?;
    let context = Context::of(&recording);
    match command.as_str() {
        "save" => {
            let out = BufWriter::new(File::create(other)?);
            write_table(out, &context, &annotations)?;
        }
        "load" => {
            let path = Path::new(other);
            let format = Format::of(path).context("not a .tsv or .csv file")?;
            let loaded = read(File::open(path)?, format, &context)?;
            let mut json = String::from("{\"annotations\": [");
            for (k, a) in loaded.annotations.iter().enumerate() {
                let sep = if k == 0 { "" } else { "," };
                let channels: Vec<String> = a
                    .channels
                    .iter()
                    .map(|&c| string(&recording.channel(c).name))
                    .collect();
                let _ = write!(
                    json,
                    "{sep}\n  {{\"onset\": {}, \"duration\": {}, \"description\": {}, \
                     \"channels\": [{}]}}",
                    a.onset,
                    a.duration,
                    string(&a.description),
                    channels.join(", ")
                );
            }
            let _ = write!(
                json,
                "\n], \"outside\": {}, \"unknown_channels\": {}}}",
                loaded.outside, loaded.unknown_channels
            );
            println!("{json}");
        }
        _ => bail!("unknown command {command:?}: save or load"),
    }
    Ok(())
}

/// `s` as a JSON string.
fn string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
