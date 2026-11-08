//! Synthetic EDF/BDF files for tests and benchmarks.
//!
//! A [`Spec`] describes a file: format, channels (each with its own rate and scaling),
//! record layout, gaps (EDF+D/BDF+D) and TAL annotations (EDF+/BDF+). [`write()`] streams it to
//! disk. The sample values come from a deterministic truth function, [`Spec::digital`], so tests
//! can check any reader output without keeping the data in memory.
//!
//! The truth function is a sine plus a sawtooth ramp plus seeded hash noise, together within
//! each channel's [`amplitude`](ChannelSpec::amplitude) of the middle of its digital range (15 uV
//! for EEG, so traces fit MNE's default scale), plus sparse **single-sample spikes** at the
//! digital minimum or maximum. Spikes are the only samples that reach the digital limits, so a
//! min/max pyramid that drops a peak is caught at once. Spikes are rare by default
//! ([`Spec::spike_every`]), so they don't cover the traces in a viewer.

mod header;
mod signal;
mod temp;
mod write;

use std::ops::Range;

pub use temp::{TempDir, TempFile};
pub use write::{write, write_to};

/// File format to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// Plain EDF: no annotation signal.
    Edf,
    EdfPlusC,
    EdfPlusD,
    /// Plain BDF (BioSemi, 24-bit).
    Bdf,
    BdfPlusC,
    BdfPlusD,
}

impl Format {
    pub const ALL: [Format; 6] = [
        Format::Edf,
        Format::EdfPlusC,
        Format::EdfPlusD,
        Format::Bdf,
        Format::BdfPlusC,
        Format::BdfPlusD,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Format::Edf => "EDF",
            Format::EdfPlusC => "EDF+C",
            Format::EdfPlusD => "EDF+D",
            Format::Bdf => "BDF",
            Format::BdfPlusC => "BDF+C",
            Format::BdfPlusD => "BDF+D",
        }
    }

    /// Parses a name as returned by [`name`](Self::name), case-insensitively.
    pub fn parse(name: &str) -> Option<Format> {
        Format::ALL
            .into_iter()
            .find(|f| f.name().eq_ignore_ascii_case(name))
    }

    pub fn is_bdf(self) -> bool {
        matches!(self, Format::Bdf | Format::BdfPlusC | Format::BdfPlusD)
    }

    /// EDF+ or BDF+: has an annotation signal with a time-keeping TAL in every record.
    pub fn is_plus(self) -> bool {
        !matches!(self, Format::Edf | Format::Bdf)
    }

    /// EDF+D or BDF+D: records may be separated by gaps.
    pub fn is_discontinuous(self) -> bool {
        matches!(self, Format::EdfPlusD | Format::BdfPlusD)
    }

    /// Bytes per stored sample: 2 (int16) or 3 (int24).
    pub fn bytes_per_sample(self) -> usize {
        if self.is_bdf() { 3 } else { 2 }
    }

    /// The range of the stored integer type.
    pub fn digital_limits(self) -> (i32, i32) {
        if self.is_bdf() {
            (-8_388_608, 8_388_607)
        } else {
            (-32_768, 32_767)
        }
    }

    /// File extension, `"edf"` or `"bdf"`.
    pub fn extension(self) -> &'static str {
        if self.is_bdf() { "bdf" } else { "edf" }
    }
}

/// Default [`Spec::spike_every`]: about one spike per channel every 50 s at 2048 Hz.
pub const SPIKE_EVERY: u64 = 100_000;

/// How far EEG channels reach from zero, in uV: within MNE's default EEG scale (20 uV per
/// half channel spacing), so traces show at Kymo's default zoom.
pub const EEG_AMPLITUDE: f64 = 15.0;

/// One data signal. Numbers are written with Rust's shortest round-trip formatting, so they
/// must fit their 8-character header field exactly as given.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelSpec {
    pub label: String,
    pub transducer: String,
    /// Physical dimension, e.g. `"uV"`.
    pub dimension: String,
    /// May exceed `physical_max`, which makes the gain negative.
    pub physical_min: f64,
    pub physical_max: f64,
    pub digital_min: i32,
    pub digital_max: i32,
    pub prefilter: String,
    /// Samples per data record; the rate is this over the record duration.
    pub samples_per_record: u32,
    /// A trigger channel: pulses of codes 1 to 255 instead of the EEG-like signal (see
    /// [`ChannelSpec::trigger`]).
    pub trigger: bool,
    /// How far the signal (sine, ramp and noise) reaches from the middle of the digital
    /// range, in physical units; at most 60% of the half-range. Spikes still reach the
    /// digital limits. Unused for trigger channels.
    pub amplitude: f64,
}

impl ChannelSpec {
    /// An EEG channel with the usual full-range scaling of the format:
    /// 0.1 uV per bit for EDF, about 0.03125 uV per bit for BDF (as BioSemi). Its signal
    /// stays within [`EEG_AMPLITUDE`] of zero, as real EEG does.
    pub fn eeg(label: impl Into<String>, samples_per_record: u32, format: Format) -> Self {
        let (digital_min, digital_max) = format.digital_limits();
        let (physical_min, physical_max) = if format.is_bdf() {
            (-262_144.0, 262_143.0)
        } else {
            (-3276.8, 3276.7)
        };
        ChannelSpec {
            label: label.into(),
            transducer: "AgAgCl electrode".into(),
            dimension: "uV".into(),
            physical_min,
            physical_max,
            digital_min,
            digital_max,
            prefilter: "HP:0.1Hz LP:100Hz".into(),
            samples_per_record,
            trigger: false,
            amplitude: EEG_AMPLITUDE,
        }
    }

    /// A trigger channel with unit scaling (physical = digital), named as readers expect:
    /// "Status" for BDF, "TRIGGER" for EDF.
    ///
    /// Every half second holds, depending on a hash: nothing, a pulse of one code (1 to 255,
    /// one sample to an eighth of a second long), or two codes back to back without a 0
    /// between them. The first pulse may start at sample 0. In BDF, bits 16 to 23 carry
    /// status bits as BioSemi's do: bit 20 always set, bit 16 toggling every 1.5 s, so the
    /// raw value is never 0 and only its low 16 bits hold the codes.
    pub fn trigger(samples_per_record: u32, format: Format) -> Self {
        let (digital_min, digital_max) = format.digital_limits();
        ChannelSpec {
            label: if format.is_bdf() { "Status" } else { "TRIGGER" }.into(),
            transducer: "Triggers and Status".into(),
            dimension: "Boolean".into(),
            physical_min: f64::from(digital_min),
            physical_max: f64::from(digital_max),
            digital_min,
            digital_max,
            prefilter: "No filtering".into(),
            samples_per_record,
            trigger: true,
            amplitude: 0.0,
        }
    }

    /// `physical = digital * gain + offset`, as EDF defines it.
    pub fn gain(&self) -> f64 {
        (self.physical_max - self.physical_min)
            / (f64::from(self.digital_max) - f64::from(self.digital_min))
    }

    pub fn offset(&self) -> f64 {
        self.physical_min - self.gain() * f64::from(self.digital_min)
    }
}

/// A gap before a data record (EDF+D/BDF+D only).
#[derive(Clone, Debug, PartialEq)]
pub struct GapSpec {
    /// The record the gap precedes, `1..n_records`.
    pub before_record: u64,
    /// Length of the gap in seconds, `> 0`.
    pub seconds: f64,
}

/// One TAL (time-stamped annotation list) entry: one or more texts sharing onset and duration.
#[derive(Clone, Debug, PartialEq)]
pub struct Tal {
    /// Seconds since the start date/time of the file (not since the first record).
    pub onset: f64,
    /// `None` writes no duration field.
    pub duration: Option<f64>,
    /// UTF-8 texts; each becomes one annotation.
    pub texts: Vec<String>,
}

/// Errors of the generator.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid spec: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A complete description of a synthetic file.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub format: Format,
    /// Start date and time; the year must be in 1985..=2084 (EDF's two-digit year).
    pub start: jiff::civil::DateTime,
    /// Local patient identification. EDF+ expects four subfields, e.g. `"X X X X"`.
    pub patient: String,
    /// Local recording identification. For EDF+/BDF+ the writer prepends
    /// `"Startdate dd-MMM-yyyy "`, so this holds the remaining subfields, e.g. `"X X X"`.
    pub recording: String,
    /// Seconds; written as is, so it must fit 8 characters.
    pub record_duration: f64,
    pub n_records: u64,
    /// EDF+/BDF+: onset of the first record after the start time, in seconds (e.g. a
    /// sub-second start). Must be 0 for plain EDF/BDF.
    pub first_onset: f64,
    pub channels: Vec<ChannelSpec>,
    /// Sorted by record; EDF+D/BDF+D only.
    pub gaps: Vec<GapSpec>,
    /// EDF+/BDF+ only. Each goes into the record whose time span holds its onset (the first or
    /// last record if outside all of them, the preceding record if in a gap).
    pub annotations: Vec<Tal>,
    /// Seeds the noise and spike positions.
    pub seed: u64,
    /// One sample in this many is a spike, on average; 0 for none. The default,
    /// [`SPIKE_EVERY`], keeps them rare on screen; tests that hunt peaks set it lower.
    pub spike_every: u64,
}

impl Spec {
    /// A spec with default identification, start 2024-01-02 03:04:05, no gaps, no annotations.
    pub fn new(
        format: Format,
        channels: Vec<ChannelSpec>,
        record_duration: f64,
        n_records: u64,
    ) -> Self {
        Spec {
            format,
            start: jiff::civil::date(2024, 1, 2).at(3, 4, 5, 0),
            patient: "X X X X".into(),
            recording: if format.is_plus() {
                "X X X".into()
            } else {
                "kymo-testgen".into()
            },
            record_duration,
            n_records,
            first_onset: 0.0,
            channels,
            gaps: Vec::new(),
            annotations: Vec::new(),
            seed: 0,
            spike_every: SPIKE_EVERY,
        }
    }

    /// Checks everything the writer needs; [`write()`] calls it.
    pub fn validate(&self) -> Result<(), Error> {
        header::validate(self)
    }

    /// Sampling rate of a data channel in Hz.
    pub fn rate(&self, channel: usize) -> f64 {
        f64::from(self.channels[channel].samples_per_record) / self.record_duration
    }

    /// Onset of a record in seconds since the start time, as written in its time-keeping TAL.
    pub fn record_onset(&self, record: u64) -> f64 {
        let gaps: f64 = self
            .gaps
            .iter()
            .take_while(|g| g.before_record <= record)
            .fold(0.0, |sum, g| sum + g.seconds);
        onset(self.first_onset, record, self.record_duration, gaps)
    }

    /// Index of the first sample of `record` in `channel`'s sample grid.
    ///
    /// The grid starts at the first record. Without gaps, record `r` starts at
    /// `r * samples_per_record`. In EDF+D/BDF+D it starts at
    /// `round((onset(r) - onset(0)) * samples_per_record / record_duration)`: record onsets are
    /// rounded to the nearest sample, and samples between records are gaps.
    pub fn record_start(&self, channel: usize, record: u64) -> u64 {
        let spr = self.channels[channel].samples_per_record;
        if self.format.is_discontinuous() {
            let elapsed = self.record_onset(record) - self.record_onset(0);
            grid_start(elapsed, spr, self.record_duration)
        } else {
            record * u64::from(spr)
        }
    }

    /// Length of `channel`'s sample grid, gaps included.
    pub fn len(&self, channel: usize) -> u64 {
        match self.n_records {
            0 => 0,
            n => {
                self.record_start(channel, n - 1)
                    + u64::from(self.channels[channel].samples_per_record)
            }
        }
    }

    /// The sample ranges of `channel` that hold data: sorted, disjoint and not touching.
    pub fn segments(&self, channel: usize) -> Vec<Range<u64>> {
        let spr = u64::from(self.channels[channel].samples_per_record);
        let mut segments: Vec<Range<u64>> = Vec::new();
        let mut starts = std::iter::once(0)
            .chain(self.gaps.iter().map(|g| g.before_record))
            .chain(std::iter::once(self.n_records))
            .peekable();
        while let (Some(first), Some(&next)) = (starts.next(), starts.peek()) {
            if first < next {
                let end = self.record_start(channel, next - 1) + spr;
                let segment = self.record_start(channel, first)..end;
                // A gap shorter than half a sample rounds away.
                match segments.last_mut() {
                    Some(last) if last.end == segment.start => last.end = segment.end,
                    _ => segments.push(segment),
                }
            }
        }
        segments
    }

    /// The truth function: digital value of `channel` at grid sample `i`.
    ///
    /// Defined for every `i`; in gaps no value is stored.
    pub fn digital(&self, channel: usize, i: u64) -> i32 {
        let signal = signal::ChannelSignal::new(self, channel);
        signal.value(i, signal.sine(i % signal.sine_period))
    }

    /// The trigger code (the low 16 bits) of a trigger channel at `i`; `None` for other
    /// channels.
    pub fn trigger_code(&self, channel: usize, i: u64) -> Option<i32> {
        signal::ChannelSignal::new(self, channel).code(i)
    }

    /// Whether the truth function has a spike at `i` (a value at the digital min or max).
    pub fn is_spike(&self, channel: usize, i: u64) -> bool {
        signal::ChannelSignal::new(self, channel).spike(i).is_some()
    }

    /// `digital(channel, i) * gain + offset`, in f64.
    pub fn physical(&self, channel: usize, i: u64) -> f64 {
        let c = &self.channels[channel];
        f64::from(self.digital(channel, i)) * c.gain() + c.offset()
    }

    /// Size of the header in bytes: 256 per signal plus 256, annotation signal included.
    pub fn header_bytes(&self) -> u64 {
        256 * (self.signal_count() as u64 + 1)
    }

    /// Size of one data record in bytes.
    pub fn record_bytes(&self) -> Result<u64, Error> {
        Ok(write::Layout::new(self)?.record_bytes as u64)
    }

    /// Size of the whole file in bytes.
    pub fn file_size(&self) -> Result<u64, Error> {
        Ok(self.header_bytes() + self.n_records * self.record_bytes()?)
    }

    /// Number of signals in the file: the data channels plus the annotation signal, if any.
    pub fn signal_count(&self) -> usize {
        self.channels.len() + usize::from(self.format.is_plus())
    }

    /// Samples per record of the annotation signal (EDF+/BDF+), sized to fit the largest TAL
    /// list of any record.
    pub fn annotation_samples_per_record(&self) -> Result<Option<u32>, Error> {
        Ok(write::Layout::new(self)?.annotation_spr)
    }

    /// A named preset (see [`PRESETS`]).
    pub fn preset(name: &str) -> Option<Spec> {
        presets::get(name)
    }
}

/// Names and descriptions of the presets.
pub const PRESETS: &[(&str, &str)] = presets::LIST;

fn onset(first_onset: f64, record: u64, record_duration: f64, gaps: f64) -> f64 {
    first_onset + record as f64 * record_duration + gaps
}

fn grid_start(elapsed: f64, samples_per_record: u32, record_duration: f64) -> u64 {
    (elapsed * f64::from(samples_per_record) / record_duration).round() as u64
}

mod presets {
    use super::{ChannelSpec, EEG_AMPLITUDE, Format, GapSpec, Spec, Tal};

    pub const LIST: &[(&str, &str)] = &[
        (
            "edf-64ch-1h",
            "EDF, 64 EEG channels at 1000 Hz, 1 h (461 MB)",
        ),
        (
            "edf-306ch-1h",
            "EDF, 306 EEG channels at 1000 Hz, 1 h, standing in for a 306-channel MEG \
             recording (2 GB)",
        ),
        (
            "bdf-256ch-8h",
            "BDF, 256 EEG channels at 2048 Hz, 8 h (45 GB)",
        ),
        (
            "edf+d-mixed",
            "EDF+D, 7 channels at 256/512/64 Hz, 10 min with 3 gaps, annotations, \
             huge offset, negative gain and a trigger channel",
        ),
        (
            "bdf+c-small",
            "BDF+C, 5 channels at 512 Hz, 1 min, annotations, negative gain and a Status \
             channel",
        ),
        (
            "edf+c-annotations",
            "EDF+C, 32 EEG channels at 256 Hz, 1 h, 10,000 annotations of 8 descriptions, \
             overlapping, a fifth of them events (59 MB)",
        ),
    ];

    pub fn get(name: &str) -> Option<Spec> {
        let spec = match name {
            "edf-64ch-1h" => eeg(Format::Edf, 64, 1000, 1.0, 3600),
            "edf-306ch-1h" => eeg(Format::Edf, 306, 1000, 1.0, 3600),
            "bdf-256ch-8h" => eeg(Format::Bdf, 256, 2048, 1.0, 8 * 3600),
            "edf+d-mixed" => edf_d_mixed(),
            "bdf+c-small" => bdf_c_small(),
            "edf+c-annotations" => edf_c_annotations(),
            _ => return None,
        };
        Some(spec)
    }

    fn eeg(format: Format, n: usize, spr: u32, record_duration: f64, records: u64) -> Spec {
        let channels = (1..=n)
            .map(|i| ChannelSpec::eeg(format!("EEG {i:03}"), spr, format))
            .collect();
        Spec::new(format, channels, record_duration, records)
    }

    fn edf_d_mixed() -> Spec {
        let f = Format::EdfPlusD;
        let mut channels: Vec<ChannelSpec> = ["Fz", "Cz", "Pz", "Oz"]
            .iter()
            .map(|l| ChannelSpec::eeg(format!("EEG {l}"), 128, f))
            .collect();
        channels.push(ChannelSpec {
            // A large DC offset: 1e6 uV with 0.0153 uV per bit.
            label: "EEG DC".into(),
            physical_min: 999_500.0,
            physical_max: 1_000_500.0,
            ..ChannelSpec::eeg("", 256, f)
        });
        channels.push(ChannelSpec {
            label: "Resp".into(),
            transducer: "thermistor".into(),
            dimension: "mV".into(),
            physical_min: 5.0,
            physical_max: -5.0,
            // In mV: as large as EEG on screen, as both count as EEG there.
            amplitude: EEG_AMPLITUDE / 1000.0,
            ..ChannelSpec::eeg("", 32, f)
        });
        channels.push(ChannelSpec::trigger(256, f));
        let mut spec = Spec::new(f, channels, 0.5, 1200);
        spec.first_onset = 0.25;
        spec.gaps = vec![
            GapSpec {
                before_record: 200,
                seconds: 1.5,
            },
            GapSpec {
                before_record: 500,
                seconds: 10.0,
            },
            GapSpec {
                before_record: 900,
                seconds: 0.25,
            },
        ];
        spec.annotations = (0..80)
            .map(|k| {
                let onset = 0.25 + f64::from(k) * 7.375;
                match k % 4 {
                    0 => Tal {
                        onset,
                        duration: None,
                        texts: vec![format!("event {k}")],
                    },
                    1 => Tal {
                        onset,
                        duration: Some(2.5),
                        texts: vec!["sleep stage N2".into()],
                    },
                    2 => Tal {
                        onset,
                        duration: Some(0.0),
                        texts: vec!["eyes closed".into(), "Elektrodenprüfung µV".into()],
                    },
                    _ => Tal {
                        onset,
                        duration: Some(0.125),
                        texts: vec![format!("stim {}", k % 7)],
                    },
                }
            })
            .collect();
        spec.seed = 7;
        spec
    }

    /// Many annotations, for the annotation editing benchmark: one every 0.36 s, shifted
    /// a little, up to 4.2 s long, so many overlap.
    fn edf_c_annotations() -> Spec {
        const DESCRIPTIONS: [&str; 8] = [
            "BAD",
            "blink",
            "saccade",
            "spindle",
            "K-complex",
            "arousal",
            "T0",
            "T1",
        ];
        let mut spec = eeg(Format::EdfPlusC, 32, 256, 1.0, 3600);
        spec.annotations = (0..10_000u32)
            .map(|k| Tal {
                onset: f64::from(k) * 0.36 + f64::from(k * 37 % 30) / 100.0,
                duration: (k % 5 != 0).then(|| f64::from(k % 7) * 0.7),
                texts: vec![DESCRIPTIONS[k as usize % DESCRIPTIONS.len()].into()],
            })
            .collect();
        spec
    }

    fn bdf_c_small() -> Spec {
        let f = Format::BdfPlusC;
        let mut channels: Vec<ChannelSpec> = ["A1", "A2", "A3"]
            .iter()
            .map(|l| ChannelSpec::eeg(*l, 512, f))
            .collect();
        channels.push(ChannelSpec {
            label: "EXG1".into(),
            physical_min: 262_143.0,
            physical_max: -262_144.0,
            ..ChannelSpec::eeg("", 512, f)
        });
        channels.push(ChannelSpec::trigger(512, f));
        let mut spec = Spec::new(f, channels, 1.0, 60);
        spec.annotations = (0..12)
            .map(|k| Tal {
                onset: f64::from(k) * 5.0 + 0.5,
                duration: (k % 2 == 1).then_some(1.0),
                texts: vec![format!("trigger {k}")],
            })
            .collect();
        spec.seed = 3;
        spec
    }
}
