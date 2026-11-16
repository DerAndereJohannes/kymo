//! Native sample types and typed sample buffers.

use crate::pyramid::BlockSum;
use crate::tile::{TILE_LEN, TileData};

/// In-memory type of raw samples, as stored in the pyramid.
///
/// File encodings stay inside the reader: BDF's int24 is widened to `I32`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SampleType {
    I16,
    I32,
    F32,
}

impl SampleType {
    /// Size of one sample in bytes.
    pub fn size(self) -> usize {
        match self {
            SampleType::I16 => 2,
            SampleType::I32 | SampleType::F32 => 4,
        }
    }
}

/// A mutable buffer of native samples, channel-major: `buf[c * len + i]`.
#[derive(Debug)]
pub enum SamplesMut<'a> {
    I16(&'a mut [i16]),
    I32(&'a mut [i32]),
    F32(&'a mut [f32]),
}

impl SamplesMut<'_> {
    pub fn sample_type(&self) -> SampleType {
        match self {
            SamplesMut::I16(_) => SampleType::I16,
            SamplesMut::I32(_) => SampleType::I32,
            SamplesMut::F32(_) => SampleType::F32,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            SamplesMut::I16(b) => b.len(),
            SamplesMut::I32(b) => b.len(),
            SamplesMut::F32(b) => b.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for i16 {}
    impl Sealed for i32 {}
    impl Sealed for f32 {}
}

/// A native sample type (`i16`, `i32` or `f32`).
///
/// Lets pyramid and tile code be generic and dispatch on [`SampleType`] once at the top.
pub trait Sample: sealed::Sealed + Copy + Default + PartialOrd + Send + Sync + 'static {
    const TYPE: SampleType;
    /// Initial minimum of a bucket. An empty bucket keeps `min > max`.
    const EMPTY_MIN: Self;
    /// Initial maximum of a bucket.
    const EMPTY_MAX: Self;

    fn to_f64(self) -> f64;

    /// Wraps a typed slice as a [`SamplesMut`].
    fn as_samples_mut(buf: &mut [Self]) -> SamplesMut<'_>;

    /// The typed slice inside `samples`, or `None` if it holds a different type.
    fn from_samples_mut(samples: SamplesMut<'_>) -> Option<&mut [Self]>;
}

macro_rules! impl_sample {
    ($t:ty, $variant:ident, $empty_min:expr, $empty_max:expr) => {
        impl Sample for $t {
            const TYPE: SampleType = SampleType::$variant;
            const EMPTY_MIN: Self = $empty_min;
            const EMPTY_MAX: Self = $empty_max;

            fn to_f64(self) -> f64 {
                f64::from(self)
            }

            fn as_samples_mut(buf: &mut [Self]) -> SamplesMut<'_> {
                SamplesMut::$variant(buf)
            }

            fn from_samples_mut(samples: SamplesMut<'_>) -> Option<&mut [Self]> {
                match samples {
                    SamplesMut::$variant(buf) => Some(buf),
                    _ => None,
                }
            }
        }
    };
}

impl_sample!(i16, I16, i16::MAX, i16::MIN);
impl_sample!(i32, I32, i32::MAX, i32::MIN);
impl_sample!(f32, F32, f32::INFINITY, f32::NEG_INFINITY);

/// What tiles, the pyramid build and stim scans need of each native sample type, one
/// implementation per type so that the build's kernels vectorize.
pub(crate) trait NativeSample: Sample {
    fn into_data(values: Box<[Self]>) -> TileData;
    /// The value with only the bits of `mask` kept, as a stim code.
    fn masked(self, mask: u32) -> Self;
    /// The value as a stim code.
    fn code(self) -> i32;
    /// `acc` plus the sum of `xs`, which lie inside one block. Exact for integers; for
    /// floats the samples are added one by one, in order, missing ones (NaN or infinite)
    /// left out and counted.
    fn sum(xs: &[Self], acc: BlockSum) -> BlockSum;
    /// `(min, max)` extended by `xs`. A missing value (NaN or infinite) never becomes a min or
    /// max.
    fn min_max(xs: &[Self], min: Self, max: Self) -> (Self, Self);
}

impl NativeSample for i16 {
    fn into_data(values: Box<[Self]>) -> TileData {
        TileData::I16(values)
    }

    fn masked(self, mask: u32) -> Self {
        // Reinterpreting the bits: the mask applies to the two's-complement value.
        self & mask as i16
    }

    fn code(self) -> i32 {
        i32::from(self)
    }

    fn sum(xs: &[i16], acc: BlockSum) -> BlockSum {
        debug_assert!(xs.len() <= TILE_LEN as usize);
        // At most 4096 · 2^15 = 2^27 in magnitude: exact in i32, then exact in f64.
        BlockSum {
            sum: acc.sum + f64::from(xs.iter().map(|&x| i32::from(x)).sum::<i32>()),
            missing: acc.missing,
        }
    }

    fn min_max(xs: &[i16], min: i16, max: i16) -> (i16, i16) {
        let lo = xs.iter().copied().fold(min, Ord::min);
        let hi = xs.iter().copied().fold(max, Ord::max);
        (lo, hi)
    }
}

impl NativeSample for i32 {
    fn into_data(values: Box<[Self]>) -> TileData {
        TileData::I32(values)
    }

    fn masked(self, mask: u32) -> Self {
        self & mask as i32
    }

    fn code(self) -> i32 {
        self
    }

    fn sum(xs: &[i32], acc: BlockSum) -> BlockSum {
        debug_assert!(xs.len() <= TILE_LEN as usize);
        // At most 4096 · 2^31 = 2^43 in magnitude: exact in i64 and in f64.
        BlockSum {
            sum: acc.sum + xs.iter().map(|&x| i64::from(x)).sum::<i64>() as f64,
            missing: acc.missing,
        }
    }

    fn min_max(xs: &[i32], min: i32, max: i32) -> (i32, i32) {
        let lo = xs.iter().copied().fold(min, Ord::min);
        let hi = xs.iter().copied().fold(max, Ord::max);
        (lo, hi)
    }
}

impl NativeSample for f32 {
    fn into_data(values: Box<[Self]>) -> TileData {
        TileData::F32(values)
    }

    /// Truncated toward zero and saturated as an integer first; a missing value (NaN or
    /// infinite) stays as it is.
    fn masked(self, mask: u32) -> Self {
        if self.is_finite() {
            (self as i32 & mask as i32) as f32
        } else {
            self
        }
    }

    /// Truncated toward zero and saturated; a missing value (NaN or infinite) is 0.
    fn code(self) -> i32 {
        if self.is_finite() { self as i32 } else { 0 }
    }

    fn sum(xs: &[f32], acc: BlockSum) -> BlockSum {
        xs.iter().fold(acc, |acc, &x| {
            if x.is_finite() {
                BlockSum {
                    sum: acc.sum + f64::from(x),
                    ..acc
                }
            } else {
                BlockSum {
                    missing: acc.missing + 1,
                    ..acc
                }
            }
        })
    }

    fn min_max(xs: &[f32], min: f32, max: f32) -> (f32, f32) {
        // NaN compares false either way and never replaces a bound. Infinities (values beyond
        // float32's range, missing as NaN is) would, and an infinite bound would hide the
        // bucket's finite samples, as the renderer skips it. They are rare, so the plain fold
        // (which vectorizes) runs first, and only a bucket it finds infinite is folded again
        // without them. The bounds passed in are finite or the empty sentinels (+∞, −∞), so
        // only a sample makes the minimum −∞ or the maximum +∞.
        let (lo, hi) = xs.iter().fold((min, max), |(lo, hi), &x| {
            (if x < lo { x } else { lo }, if x > hi { x } else { hi })
        });
        if lo > f32::NEG_INFINITY && hi < f32::INFINITY {
            return (lo, hi);
        }
        xs.iter().fold((min, max), |(lo, hi), &x| {
            (
                if x < lo && x > f32::NEG_INFINITY {
                    x
                } else {
                    lo
                },
                if x > hi && x < f32::INFINITY { x } else { hi },
            )
        })
    }
}
