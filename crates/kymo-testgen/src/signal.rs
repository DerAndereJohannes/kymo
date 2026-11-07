//! The deterministic truth function behind [`Spec::digital`](crate::Spec::digital).

use std::f64::consts::TAU;

use crate::Spec;

/// One in this many samples is a spike (on average).
const SPIKE_EVERY: u64 = 997;

/// Parameters of one channel's synthetic signal.
pub(crate) struct ChannelSignal {
    key: u64,
    mid: f64,
    half: f64,
    min: i32,
    max: i32,
    /// Whole samples, so the writer can use a lookup table.
    pub(crate) sine_period: u64,
    ramp_period: u64,
    /// For trigger channels: samples per half-second slot, and whether status bits are set.
    trigger: Option<(u64, bool)>,
}

impl ChannelSignal {
    pub(crate) fn new(spec: &Spec, channel: usize) -> Self {
        let c = &spec.channels[channel];
        let rate = spec.rate(channel);
        // 2 to 10 Hz, depending on the channel.
        let frequency = 2.0 + (channel % 9) as f64;
        let (min, max) = (c.digital_min, c.digital_max);
        ChannelSignal {
            key: splitmix64(spec.seed ^ splitmix64(channel as u64)),
            mid: (f64::from(min) + f64::from(max)) / 2.0,
            half: (f64::from(max) - f64::from(min)) / 2.0,
            min,
            max,
            sine_period: ((rate / frequency).round() as u64).max(4),
            ramp_period: ((rate * 20.0).round() as u64).max(2),
            trigger: c
                .trigger
                .then(|| (((rate / 2.0).round() as u64).max(8), spec.format.is_bdf())),
        }
    }

    /// The trigger code at `i` (see [`ChannelSpec::trigger`](crate::ChannelSpec::trigger)),
    /// if this is a trigger channel.
    pub(crate) fn code(&self, i: u64) -> Option<i32> {
        let (slot, _) = self.trigger?;
        let (k, j) = (i / slot, i % slot);
        let h = splitmix64(self.key ^ k.wrapping_mul(0x9E37_79B9));
        let first = 1 + (h >> 8) % 255;
        let second = 1 + (first + (h >> 16) % 254) % 255;
        let len = 1 + (h >> 24) % (slot / 4);
        let code = match h % 8 {
            0..=3 if j < len => first,
            4 | 5 if j < len => first,
            4 | 5 if j < 2 * len => second,
            _ => 0,
        };
        Some(code as i32)
    }

    /// The sine at phase `k` of `sine_period` (what the writer tabulates).
    pub(crate) fn sine(&self, k: u64) -> f64 {
        (TAU * k as f64 / self.sine_period as f64).sin()
    }

    /// The spike value at `i`, if any. Trigger channels have none.
    pub(crate) fn spike(&self, i: u64) -> Option<i32> {
        if self.trigger.is_some() {
            return None;
        }
        self.spike_from(splitmix64(self.key ^ i))
    }

    fn spike_from(&self, h: u64) -> Option<i32> {
        let s = splitmix64(h);
        s.is_multiple_of(SPIKE_EVERY)
            .then_some(if s & (1 << 40) != 0 {
                self.max
            } else {
                self.min
            })
    }

    /// The value at `i`, given `sine = self.sine(i % self.sine_period)`.
    pub(crate) fn value(&self, i: u64, sine: f64) -> i32 {
        if let (Some((slot, status)), Some(code)) = (self.trigger, self.code(i)) {
            let bits = if status {
                (1 << 20) | (((i / (3 * slot)) % 2) << 16) as i32
            } else {
                0
            };
            return code | bits;
        }
        let h = splitmix64(self.key ^ i);
        if let Some(spike) = self.spike_from(h) {
            return spike;
        }
        let noise = (h >> 11) as f64 * (2.0 / (1u64 << 53) as f64) - 1.0;
        let ramp = 2.0 * (i % self.ramp_period) as f64 / self.ramp_period as f64 - 1.0;
        // At most 0.6 of the half-range, so only spikes reach the limits.
        let x = self.mid + self.half * (0.35 * sine + 0.15 * ramp + 0.1 * noise);
        (x.round() as i32).clamp(self.min + 1, self.max - 1)
    }
}

/// SplitMix64: a fast, well-mixed 64-bit hash.
fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
