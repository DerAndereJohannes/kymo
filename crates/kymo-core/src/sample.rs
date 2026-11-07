//! Native sample types and typed sample buffers.

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
