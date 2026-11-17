//! Saves and loads a BIDS `channels.tsv` as the viewer does, for comparison with MNE-BIDS
//! (`scripts/compare_mne.py --channels`).
//!
//! ```text
//! cargo run --release -p kymo-core --example channels -- save FILE OUT.tsv
//! cargo run --release -p kymo-core --example channels -- load FILE IN.tsv
//! ```
//!
//! `save` writes the recording's channels with the file's types and bad channels; `load`
//! reads a `channels.tsv` for the recording and prints JSON: each channel's type after the
//! load (by name, in the file's order), its MNE type and whether it is bad, with what was
//! skipped. A channel stays stim only if the file has it stim, as in the viewer.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::{Result, bail};
use kymo_core::channel_file::{self, Context, read};
use kymo_core::{ChannelId, ChannelKind, Recording};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [command, file, other] = args.as_slice() else {
        bail!("usage: channels save|load FILE CHANNELS");
    };
    let (recording, _) = Recording::new(kymo_core::open(Path::new(file))?)?;
    let mut kinds: Vec<ChannelKind> = recording
        .channels()
        .iter()
        .map(|c| c.kind.clone())
        .collect();
    let mut bad: BTreeSet<ChannelId> = recording.bad_channels().iter().copied().collect();
    match command.as_str() {
        "save" => {
            let out = BufWriter::new(File::create(other)?);
            let entries = channel_file::entries(&recording, &kinds, |c| bad.contains(&c));
            channel_file::write(out, entries)?;
        }
        "load" => {
            let loaded = read(File::open(other)?, &Context::of(&recording))?;
            let mut refused = 0;
            for (c, kind) in &loaded.kinds {
                let i = c.0 as usize;
                if kind.is_stim() && !recording.channels()[i].kind.is_stim() {
                    refused += 1;
                } else {
                    kinds[i] = kind.clone();
                }
            }
            for &(c, is_bad) in &loaded.bad {
                if is_bad {
                    bad.insert(c);
                } else {
                    bad.remove(&c);
                }
            }
            let mut json = String::from("{\"channels\": [");
            for (i, channel) in recording.channels().iter().enumerate() {
                let sep = if i == 0 { "" } else { "," };
                let _ = write!(
                    json,
                    "{sep}\n  {{\"name\": {}, \"type\": {}, \"mne\": {}, \"bad\": {}}}",
                    string(&channel.name),
                    string(kinds[i].name()),
                    string(kinds[i].mne_name()),
                    bad.contains(&ChannelId(i as u32))
                );
            }
            let _ = write!(
                json,
                "\n], \"unknown\": {}, \"units_differ\": {}, \"refused\": {refused}}}",
                loaded.unknown, loaded.units_differ
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
