//! Per-channel metadata as stored in the file.

use crate::GridId;

/// Index of a channel in [`Recording::channels`](crate::Recording::channels).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelId(pub u32);

/// Channel type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChannelKind {
    Eeg,
    Eog,
    Ecg,
    Emg,
    Stim,
    Misc,
}

/// Physical unit of a channel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Unit {
    /// As stored in the file, e.g. `"uV"`.
    pub label: String,
    /// Factor to the SI unit, e.g. `1e-6` for `"uV"`; `None` if the label is not recognized.
    pub to_si: Option<f64>,
}

/// File metadata of one channel. Display state (visible, color, scale, bad) lives elsewhere.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub name: String,
    pub kind: ChannelKind,
    pub unit: Unit,
    /// `physical = digital * gain + offset`, computed in f64. Gain may be negative.
    pub gain: f64,
    /// Can be huge for EDF/BDF.
    pub offset: f64,
    /// The grid this channel belongs to; the sampling rate comes from there.
    pub grid: GridId,
    /// Prefiltering as stored in the file.
    pub prefilter: String,
    pub transducer: String,
    /// Physical (min, max) as stored; informational. `None` if the format has none.
    pub physical_range: Option<(f64, f64)>,
    /// Digital (min, max) as stored; `None` for float formats.
    pub digital_range: Option<(f64, f64)>,
    /// For stim channels: the bits of the raw value that carry event codes, e.g. the low 16
    /// of a BDF status channel, whose upper bits are the amplifier's own status. `None`
    /// means all bits.
    pub trigger_mask: Option<u32>,
}

impl Channel {
    /// Gain and offset that turn the values in this channel's tiles into the values shown.
    /// Tiles hold native values, so these are [`gain`](Self::gain) and
    /// [`offset`](Self::offset), except for a channel with a
    /// [`trigger_mask`](Self::trigger_mask): its tiles hold the masked codes, shown as they
    /// are (1 and 0), as MNE shows BDF stim channels.
    pub fn tile_scaling(&self) -> (f64, f64) {
        if self.trigger_mask.is_some() {
            (1.0, 0.0)
        } else {
            (self.gain, self.offset)
        }
    }

    /// The unit of the values shown from this channel's tiles: [`unit`](Self::unit)'s
    /// label, or none for a channel with a [`trigger_mask`](Self::trigger_mask), whose
    /// tiles hold codes (BioSemi files call the unit of their Status channel "Boolean").
    pub fn tile_unit(&self) -> &str {
        if self.trigger_mask.is_some() {
            ""
        } else {
            &self.unit.label
        }
    }

    /// A channel of kind `Misc` with unit gain, zero offset and empty metadata.
    pub fn new(name: impl Into<String>, grid: GridId) -> Self {
        Channel {
            name: name.into(),
            kind: ChannelKind::Misc,
            unit: Unit::default(),
            gain: 1.0,
            offset: 0.0,
            grid,
            prefilter: String::new(),
            transducer: String::new(),
            physical_range: None,
            digital_range: None,
            trigger_mask: None,
        }
    }
}
