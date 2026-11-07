//! Dumps what kymo reads from a recording, for comparison with other readers
//! (`scripts/compare_mne.py`).
//!
//! ```text
//! cargo run --release -p kymo-core --example dump -- FILE OUTDIR [--max-values N]
//! ```
//!
//! Writes to OUTDIR:
//! - `header.json`: format, start time, duration, warnings, channels and grids (rate, start,
//!   length, segments, channel ids)
//! - `annotations.json`: onset, duration and description of each annotation
//! - `windows.json`: per grid, the sample ranges that were dumped
//! - `grid{g}.f32`: for each window in turn, the physical values (little-endian f32, NaN in
//!   gaps) of all channels of grid g, channel-major
//! - `grid{g}.i32`: the same samples as raw digital values (little-endian i32, 0 in gaps)
//! - `events.json`: per stim channel, its events (sample and code) under the default rule
//!   (every change to a non-zero code, none at the first sample), from a pyramid build
//!
//! A grid with at most N values (channels × samples, default 20 million) is dumped whole;
//! otherwise 16 windows, the first at the start, the last at the end, the others spread
//! deterministically in between.

use std::fmt::Write as _;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use kymo_core::pyramid::Pyramid;
use kymo_core::stim::{self, EventOptions};
use kymo_core::{GridId, Recording, SampleType, SamplesMut};

const WINDOWS: u64 = 16;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (file, out, max_values) = match args.as_slice() {
        [file, out] => (file, out, 20_000_000),
        [file, out, flag, n] if flag == "--max-values" => {
            (file, out, n.parse().context("invalid --max-values")?)
        }
        _ => bail!("usage: dump FILE OUTDIR [--max-values N]"),
    };
    let out = PathBuf::from(out);
    fs::create_dir_all(&out)?;

    let opened = kymo_core::open(Path::new(file))?;
    let (rec, annotations) = Recording::new(opened)?;

    fs::write(out.join("header.json"), header_json(&rec))?;

    let mut json = String::from("[");
    for (k, a) in annotations.iter().enumerate() {
        let sep = if k == 0 { "" } else { "," };
        let _ = write!(
            json,
            "{sep}\n  {{\"onset\": {}, \"duration\": {}, \"description\": {}}}",
            a.onset,
            a.duration,
            string(&a.description)
        );
    }
    json.push_str("\n]\n");
    fs::write(out.join("annotations.json"), json)?;

    let mut windows_json = String::from("[");
    for (g, grid) in rec.grids().iter().enumerate() {
        let windows = windows(grid.len(), grid.channels().len() as u64, max_values);
        let ranges: Vec<String> = windows
            .iter()
            .map(|w| format!("[{}, {}]", w.start, w.end))
            .collect();
        let sep = if g == 0 { "" } else { "," };
        let _ = write!(windows_json, "{sep}\n  [{}]", ranges.join(", "));

        let create = |ext: &str| -> Result<BufWriter<fs::File>> {
            Ok(BufWriter::new(fs::File::create(
                out.join(format!("grid{g}.{ext}")),
            )?))
        };
        let (mut physical, mut native) = (create("f32")?, create("i32")?);
        let id = GridId(g as u32);
        for w in windows {
            let n = grid.channels().len() * (w.end - w.start) as usize;
            let mut values = vec![0.0f32; n];
            rec.read_physical(id, grid.channels(), w.clone(), &mut values)?;
            for v in values {
                physical.write_all(&v.to_le_bytes())?;
            }
            let digital: Vec<i32> = match grid.sample_type() {
                SampleType::I16 => {
                    let mut buf = vec![0i16; n];
                    rec.read_native(id, grid.channels(), w, SamplesMut::I16(&mut buf))?;
                    buf.into_iter().map(i32::from).collect()
                }
                SampleType::I32 => {
                    let mut buf = vec![0i32; n];
                    rec.read_native(id, grid.channels(), w, SamplesMut::I32(&mut buf))?;
                    buf
                }
                other => bail!("unexpected sample type {other:?}"),
            };
            for v in digital {
                native.write_all(&v.to_le_bytes())?;
            }
        }
        physical.flush()?;
        native.flush()?;
    }
    windows_json.push_str("\n]\n");
    fs::write(out.join("windows.json"), windows_json)?;
    fs::write(out.join("events.json"), events_json(&rec)?)?;
    Ok(())
}

/// The events of every stim channel, from a pyramid build of each grid that has one.
fn events_json(rec: &Recording) -> Result<String> {
    let mut json = String::from("[");
    for g in 0..rec.grids().len() as u32 {
        let has_stim = rec
            .grid(GridId(g))
            .channels()
            .iter()
            .any(|&c| rec.channel(c).kind == kymo_core::ChannelKind::Stim);
        if !has_stim {
            continue;
        }
        let pyramid = Pyramid::build(rec, GridId(g))?;
        for s in pyramid.stim_changes(rec) {
            let events: Vec<String> = stim::events(&s.changes, EventOptions::default())
                .iter()
                .map(|e| format!("[{}, {}]", e.sample, e.value))
                .collect();
            let sep = if json.len() > 1 { "," } else { "" };
            let _ = write!(
                json,
                "{sep}\n  {{\"channel\": {}, \"events\": [{}]}}",
                s.channel.0,
                events.join(", ")
            );
        }
    }
    json.push_str("\n]\n");
    Ok(json)
}

/// The whole grid if it fits `max_values`, else `WINDOWS` windows that do.
fn windows(len: u64, channels: u64, max_values: u64) -> Vec<std::ops::Range<u64>> {
    if len * channels <= max_values {
        return vec![0..len];
    }
    let size = (max_values / channels / WINDOWS).clamp(1, len);
    let span = len - size;
    let mut starts: Vec<u64> = (0..WINDOWS)
        .map(|k| {
            // Spread evenly, then jitter within the stride with a fixed hash.
            let base = span * k / (WINDOWS - 1);
            let jitter = (k.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) % (span / WINDOWS + 1);
            if k == 0 || k == WINDOWS - 1 {
                base
            } else {
                (base + jitter).min(span)
            }
        })
        .collect();
    starts.sort_unstable();
    starts.dedup();
    starts.into_iter().map(|s| s..s + size).collect()
}

fn header_json(rec: &Recording) -> String {
    let mut j = String::from("{\n");
    let start = rec
        .start_time()
        .map_or_else(|| "null".to_owned(), |t| string(&t.to_string()));
    let _ = writeln!(j, "  \"format\": {},", string(rec.format()));
    let _ = writeln!(j, "  \"start_time\": {start},");
    let _ = writeln!(j, "  \"duration\": {},", rec.duration());
    let warnings: Vec<String> = rec.warnings().iter().map(|w| string(w)).collect();
    let _ = writeln!(j, "  \"warnings\": [{}],", warnings.join(", "));
    j.push_str("  \"channels\": [");
    for (i, c) in rec.channels().iter().enumerate() {
        let sep = if i == 0 { "" } else { "," };
        let to_si = c
            .unit
            .to_si
            .map_or_else(|| "null".to_owned(), |x| x.to_string());
        let _ = write!(
            j,
            "{sep}\n    {{\"name\": {}, \"kind\": {}, \"unit\": {}, \"to_si\": {to_si}, \
             \"gain\": {}, \"offset\": {}, \"grid\": {}}}",
            string(&c.name),
            string(&format!("{:?}", c.kind)),
            string(&c.unit.label),
            c.gain,
            c.offset,
            c.grid.0
        );
    }
    j.push_str("\n  ],\n  \"grids\": [");
    for (g, grid) in rec.grids().iter().enumerate() {
        let sep = if g == 0 { "" } else { "," };
        let segments: Vec<String> = grid
            .segments()
            .iter()
            .map(|s| format!("[{}, {}]", s.start, s.end))
            .collect();
        let channels: Vec<String> = grid.channels().iter().map(|c| c.0.to_string()).collect();
        let _ = write!(
            j,
            "{sep}\n    {{\"rate\": {}, \"start\": {}, \"len\": {}, \"segments\": [{}], \
             \"channels\": [{}]}}",
            grid.rate(),
            grid.time_of(0),
            grid.len(),
            segments.join(", "),
            channels.join(", ")
        );
    }
    j.push_str("\n  ]\n}\n");
    j
}

/// A JSON string literal.
fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
