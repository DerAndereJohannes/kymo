//! The measurement info: channels, sampling rate, filters, date and descriptive fields.

use super::FiffError;
use super::tag::{Node, block, kind};
use crate::{ChannelKind, Unit};

/// One channel as the file describes it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ChInfo {
    pub name: String,
    /// The name in the channel info struct: [`name`](Self::name) cut to 15 bytes where an
    /// extended block holds a longer one. MNE writes bad channels under it.
    pub struct_name: String,
    /// FIFF's channel kind (1 MEG, 2 EEG, 3 stim, ...).
    pub kind: i32,
    pub range: f32,
    pub cal: f32,
    pub coil_type: i32,
    /// FIFF's unit code (107 V, 112 T, 201 T/m, ...).
    pub unit: i32,
    /// The unit's power of ten; values don't use it (MNE ignores it too).
    pub unit_mul: i32,
}

impl ChInfo {
    /// `range × cal`: physical value = stored value × calibration.
    pub fn calibration(&self) -> f64 {
        f64::from(self.range) * f64::from(self.cal)
    }

    /// The channel's type: MNE's, from its kind and, for MEG, EEG, fNIRS and eye tracking
    /// channels, its unit or coil (as MNE's `channel_type`), except that stim channels whose
    /// calibration isn't 1 are misc (trigger lines in volts, analog inputs).
    pub fn kind(&self) -> ChannelKind {
        let mne = match self.kind {
            1 if self.unit == unit::T_M => "grad",
            1 => "mag",
            2 if self.coil_type == coil::EEG_CSD => "csd",
            2 => "eeg",
            3 if self.calibration() == 1.0 => "stim",
            102 => "bio",
            202 => "eog",
            301 => "ref_meg",
            302 => "emg",
            402 => "ecg",
            602 => "resp",
            700..=709 => "chpi",
            802 => "seeg",
            803 => "dbs",
            900 => "syst",
            902 => "ecog",
            910 => "ias",
            920 => "exci",
            1000 => "dipole",
            1001 => "gof",
            1100 => match self.coil_type {
                300 => "hbo",
                301 => "hbr",
                302 => "fnirs_cw_amplitude",
                303 => "fnirs_od",
                304 => "fnirs_fd_ac_amplitude",
                305 => "fnirs_fd_phase",
                306 => "fnirs_td_gated_amplitude",
                307 => "fnirs_td_moments_intensity",
                308 => "fnirs_td_moments_mean",
                309 => "fnirs_td_moments_variance",
                _ => "misc",
            },
            1200 => "temperature",
            1300 => "gsr",
            1400 if self.coil_type == coil::EYETRACK_PUPIL => "pupil",
            1400 => "eyegaze",
            _ => "misc",
        };
        ChannelKind::named(mne)
    }

    pub fn unit(&self) -> Unit {
        Unit::from_label(unit::label(self.unit))
    }
}

/// FIFF's coil types that tell channel types apart.
mod coil {
    pub const EEG_CSD: i32 = 6;
    pub const EYETRACK_PUPIL: i32 = 401;
}

/// FIFF's unit codes.
pub(super) mod unit {
    pub const T_M: i32 = 201;

    /// The SI symbol of a unit code; empty for none, unitless and unknown codes.
    pub fn label(code: i32) -> &'static str {
        match code {
            1 => "m",
            2 => "kg",
            3 => "s",
            4 => "A",
            5 => "K",
            6 => "mol",
            7 => "rad",
            8 => "sr",
            9 => "cd",
            10 => "mol/m³",
            101 => "Hz",
            102 => "N",
            103 => "Pa",
            104 => "J",
            105 => "W",
            106 => "C",
            107 => "V",
            108 => "F",
            109 => "Ω",
            110 => "S",
            111 => "Wb",
            112 => "T",
            113 => "H",
            114 => "°C",
            115 => "lm",
            116 => "lx",
            117 => "V/m²",
            118 => "s²",
            201 => "T/m",
            202 => "Am",
            203 => "Am/m²",
            204 => "Am/m³",
            210 => "px",
            _ => "",
        }
    }
}

/// The measurement info, as kymo uses it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Info {
    pub channels: Vec<ChInfo>,
    pub sfreq: f64,
    pub lowpass: Option<f64>,
    pub highpass: Option<f64>,
    /// Seconds and microseconds since 1970 (UTC); `None` if not stored.
    pub meas_date: Option<(i64, i64)>,
    /// `+HH:MM` (or `+HHMM`, as older MNE wrote it), the local time's offset from UTC.
    pub utc_offset: Option<String>,
    pub extra: Vec<(String, String)>,
}

/// MNE's "no date": (0, 2^31 − 1).
pub(super) const DATE_NONE: (i64, i64) = (0, (1 << 31) - 1);

/// Reads the measurement info block `node`.
pub(super) fn read(node: &Node, map: &[u8], warnings: &mut Vec<String>) -> Result<Info, FiffError> {
    let mut channels = Vec::new();
    let mut nchan = None;
    let mut sfreq = None;
    let (mut lowpass, mut highpass) = (None, None);
    let (mut meas_date, mut utc_offset) = (None, None);
    let mut extra = Vec::new();
    for tag in &node.tags {
        match tag.kind {
            kind::NCHAN => nchan = tag.int(map),
            kind::SFREQ => sfreq = tag.number(map),
            kind::LOWPASS => lowpass = tag.number(map).filter(|x| x.is_finite()),
            kind::HIGHPASS => highpass = tag.number(map).filter(|x| x.is_finite()),
            kind::MEAS_DATE => {
                let mut v = tag.ints(map).map(i64::from);
                let date = (v.next(), v.next().unwrap_or(0));
                if let (Some(secs), usecs) = date
                    && (secs, usecs) != DATE_NONE
                {
                    meas_date = Some((secs, usecs));
                }
            }
            kind::UTC_OFFSET => utc_offset = Some(tag.text(map)),
            kind::CH_INFO => {
                let index = channels.len();
                channels.push(ch_info(tag.bytes(map)).ok_or(FiffError::InvalidChannel {
                    index,
                    reason: format!("its description has {} bytes, not 96", tag.size),
                })?);
            }
            kind::EXPERIMENTER | kind::DESCRIPTION | kind::PROJ_NAME => {
                let name = match tag.kind {
                    kind::EXPERIMENTER => "experimenter",
                    kind::DESCRIPTION => "description",
                    _ => "project",
                };
                let text = tag.text(map);
                if !text.is_empty() {
                    extra.push((name.to_owned(), text));
                }
            }
            kind::LINE_FREQ => {
                if let Some(f) = tag.number(map).filter(|f| f.is_finite() && *f > 0.0) {
                    extra.push(("line frequency".to_owned(), format!("{f} Hz")));
                }
            }
            _ => {}
        }
    }
    if channels.is_empty() {
        return Err(FiffError::NoChannels);
    }
    if let Some(n) = nchan
        && usize::try_from(n).ok() != Some(channels.len())
    {
        warnings.push(format!(
            "the measurement info states {n} channels but describes {}; the {} described \
             are read",
            channels.len(),
            channels.len()
        ));
    }
    extended(node, map, &mut channels, warnings);
    let sfreq =
        sfreq
            .filter(|f| f.is_finite() && *f > 0.0)
            .ok_or_else(|| FiffError::InvalidField {
                field: "sampling rate",
                value: sfreq.map_or_else(|| "missing".to_owned(), |f| f.to_string()),
            })?;
    Ok(Info {
        channels,
        sfreq,
        lowpass,
        highpass,
        meas_date,
        utc_offset,
        extra,
    })
}

/// A channel info struct: scan and logical numbers, kind, range, cal, coil type, location
/// (12 floats), unit, unit multiplier, a name of up to 16 bytes.
fn ch_info(b: &[u8]) -> Option<ChInfo> {
    let word = |k: usize| -> Option<[u8; 4]> { b.get(4 * k..4 * k + 4)?.try_into().ok() };
    let int = |k| word(k).map(i32::from_be_bytes);
    let float = |k| word(k).map(f32::from_be_bytes);
    let name = b.get(80..96)?;
    let name = name.split(|&x| x == 0).next().unwrap_or_default();
    let name = String::from_utf8_lossy(name).trim().to_owned();
    Some(ChInfo {
        struct_name: name.clone(),
        name,
        kind: int(2)?,
        range: float(3)?,
        cal: float(4)?,
        coil_type: int(5)?,
        unit: int(18)?,
        unit_mul: int(19)?,
    })
}

/// MNE's extended channel info: one block per channel, written when names are longer than
/// a channel info's 16 bytes; its fields replace the struct's.
fn extended(node: &Node, map: &[u8], channels: &mut [ChInfo], warnings: &mut Vec<String>) {
    let mut blocks = Vec::new();
    node.find_all(block::CH_INFO, &mut blocks);
    if blocks.is_empty() {
        return;
    }
    if blocks.len() != channels.len() {
        warnings.push(format!(
            "the file has extended information for {} channels but {} channels; it is \
             ignored",
            blocks.len(),
            channels.len()
        ));
        return;
    }
    for (b, ch) in blocks.iter().zip(channels.iter_mut()) {
        for tag in &b.tags {
            let int = || tag.int(map);
            let float = || tag.number(map).map(|x| x as f32);
            match tag.kind {
                kind::CH_DACQ_NAME => {
                    let name = tag.text(map);
                    if !name.is_empty() {
                        ch.name = name;
                    }
                }
                kind::CH_KIND => ch.kind = int().unwrap_or(ch.kind),
                kind::CH_RANGE => ch.range = float().unwrap_or(ch.range),
                kind::CH_CAL => ch.cal = float().unwrap_or(ch.cal),
                kind::CH_UNIT => ch.unit = int().unwrap_or(ch.unit),
                kind::CH_UNIT_MUL => ch.unit_mul = int().unwrap_or(ch.unit_mul),
                kind::CH_COIL_TYPE => ch.coil_type = int().unwrap_or(ch.coil_type),
                _ => {}
            }
        }
    }
}
