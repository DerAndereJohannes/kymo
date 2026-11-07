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
        }
    }

    /// The sine at phase `k` of `sine_period` (what the writer tabulates).
    pub(crate) fn sine(&self, k: u64) -> f64 {
        (TAU * k as f64 / self.sine_period as f64).sin()
    }

    /// The spike value at `i`, if any.
    pub(crate) fn spike(&self, i: u64) -> Option<i32> {
        self.spike_from(splitmix64(self.key ^ i))
    }

    fn spike_from(&self, h: u64) -> Option<i32> {
        let s = splitmix64(h);
        (s % SPIKE_EVERY == 0).then_some(if s & (1 << 40) != 0 {
            self.max
        } else {
            self.min
        })
    }

    /// The value at `i`, given `sine = self.sine(i % self.sine_period)`.
    pub(crate) fn value(&self, i: u64, sine: f64) -> i32 {
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
