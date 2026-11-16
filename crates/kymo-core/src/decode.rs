//! Decoding stored samples into the native sample types, shared by the readers.
//!
//! Formats store samples in many encodings; the pyramid, tiles and renderer know three
//! native types ([`SampleType`]). A [`Decoder`] turns one encoding into its native type:
//!
//! | Stored | Native | |
//! | --- | --- | --- |
//! | int8, uint8, int16 | `I16` | exact |
//! | uint16, int24, uint24, int32 | `I32` | exact |
//! | float32 | `F32` | exact |
//! | uint32, int64, uint64, float64 | `F32` | `value − reference`, rounded once to f32 |
//!
//! The reference of the last row is a per-channel value the reader picks (the channel's
//! first finite sample, see [`Decoder::reference_among`]) and folds into the channel's offset:
//! `physical = (native + reference) × gain + offset`, so the offset becomes
//! `offset + reference × gain`. A recording whose values sit far from zero then keeps their
//! variation at full f32 precision, as the renderer keeps EDF/BDF's huge offsets. Stim
//! channels keep a reference of 0: their native values are the codes.
//!
//! A decoder can also read values at or beyond two limits as missing samples, as GDF's
//! overflow detection does with its digital minimum and maximum ([`Decoder::with_limits`]).
//! Its samples then decode to `F32` with those values as NaN (integers up to 24 bits
//! exactly, int32 and wider relative to a reference).

use crate::{ReadError, Sample, SampleType, SamplesMut};

/// How many of a channel's first samples [`Decoder::reference_among`] looks at.
pub const REFERENCE_SEARCH: usize = 4096;

/// The slices starting at each of `count` samples, one every `stride` bytes from `bytes[0]`,
/// as far as `bytes` reaches.
pub fn strided(bytes: &[u8], stride: usize, count: usize) -> impl Iterator<Item = &[u8]> {
    (0..count).map_while(move |i| bytes.get(i.checked_mul(stride)?..))
}

/// Byte order of stored samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ByteOrder {
    Little,
    Big,
}

/// How a sample is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Encoding {
    I8,
    U8,
    I16,
    U16,
    I24,
    U24,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
}

impl Encoding {
    pub const ALL: [Encoding; 12] = [
        Encoding::I8,
        Encoding::U8,
        Encoding::I16,
        Encoding::U16,
        Encoding::I24,
        Encoding::U24,
        Encoding::I32,
        Encoding::U32,
        Encoding::I64,
        Encoding::U64,
        Encoding::F32,
        Encoding::F64,
    ];

    /// Bytes per stored sample.
    pub fn size(self) -> usize {
        match self {
            Encoding::I8 | Encoding::U8 => 1,
            Encoding::I16 | Encoding::U16 => 2,
            Encoding::I24 | Encoding::U24 => 3,
            Encoding::I32 | Encoding::U32 | Encoding::F32 => 4,
            Encoding::I64 | Encoding::U64 | Encoding::F64 => 8,
        }
    }

    /// The native type samples of this encoding decode to.
    pub fn native(self) -> SampleType {
        match self {
            Encoding::I8 | Encoding::U8 | Encoding::I16 => SampleType::I16,
            Encoding::U16 | Encoding::I24 | Encoding::U24 | Encoding::I32 => SampleType::I32,
            Encoding::F32 | Encoding::U32 | Encoding::I64 | Encoding::U64 | Encoding::F64 => {
                SampleType::F32
            }
        }
    }

    /// The smallest and largest value of an integer encoding; `None` for floats.
    pub fn int_range(self) -> Option<(i128, i128)> {
        let bits = 8 * self.size() as u32;
        match self {
            Encoding::F32 | Encoding::F64 => None,
            Encoding::U8 | Encoding::U16 | Encoding::U24 | Encoding::U32 | Encoding::U64 => {
                Some((0, (1i128 << bits) - 1))
            }
            _ => Some((-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)),
        }
    }

    /// Whether decoding subtracts a reference value (see the module docs). The others are
    /// exact and ignore it.
    pub fn uses_reference(self) -> bool {
        matches!(
            self,
            Encoding::U32 | Encoding::I64 | Encoding::U64 | Encoding::F64
        )
    }
}

/// Decodes every `$n`-byte sample of `$src`, one every `$stride` bytes, into `$dst` with
/// `$f`. `$src` holds exactly `$dst.len()` samples, the last one without padding after it.
///
/// A macro rather than a function, so each encoding gets its own loop: as a generic function
/// (even `#[inline(always)]`) the contiguous loop decoded BDF's int24 about 65% slower.
macro_rules! each {
    ($src:expr, $stride:expr, $dst:expr, $n:literal, $f:expr) => {{
        let f = $f;
        if $stride == $n {
            // Contiguous: arrays without a length check per sample, which vectorizes.
            for (d, s) in $dst.iter_mut().zip($src.as_chunks::<$n>().0) {
                *d = f(*s);
            }
        } else {
            // `chunks` leaves the last sample as a short piece of exactly `$n` bytes.
            for (d, s) in $dst.iter_mut().zip($src.chunks($stride)) {
                if let Some(&s) = s.first_chunk::<$n>() {
                    *d = f(s);
                }
            }
        }
    }};
}

/// Values at or beyond these limits are missing (see [`Decoder::with_limits`]).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Limits {
    low: f64,
    high: f64,
    /// The limits for integers: an integer `v` is missing if `v <= low_int` or
    /// `v >= high_int` (the limits rounded down and up).
    low_int: i128,
    high_int: i128,
}

impl Limits {
    fn int_missing(&self, v: i128) -> bool {
        v <= self.low_int || v >= self.high_int
    }

    fn float_missing(&self, v: f64) -> bool {
        v <= self.low || v >= self.high
    }
}

/// Decodes samples of one encoding and byte order into their native type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decoder {
    encoding: Encoding,
    order: ByteOrder,
    reference: f64,
    /// Values at or beyond these are missing, so samples decode to `F32`.
    limits: Option<Limits>,
}

impl Decoder {
    /// A decoder with a reference of 0.
    pub fn new(encoding: Encoding, order: ByteOrder) -> Self {
        Decoder {
            encoding,
            order,
            reference: 0.0,
            limits: None,
        }
    }

    /// The same decoder reading values `<= low` or `>= high` as missing (NaN), as biosig's
    /// overflow detection reads GDF's samples at or beyond the digital minimum and maximum,
    /// so its samples decode to `F32`: integers up to 24 bits exactly, int32 and wider
    /// relative to a reference (which int32 then [uses](Self::uses_reference)).
    ///
    /// Unchanged if the limits aren't finite with `low < high`, or (for an integer encoding)
    /// leave every value of the encoding in between.
    pub fn with_limits(self, low: f64, high: f64) -> Self {
        if !(low.is_finite() && high.is_finite() && low < high) {
            return self;
        }
        // Far beyond any 64-bit value, so the casts below can't overflow.
        let bound = 2f64.powi(100);
        let limits = Limits {
            low,
            high,
            low_int: low.floor().clamp(-bound, bound) as i128,
            high_int: high.ceil().clamp(-bound, bound) as i128,
        };
        if let Some((min, max)) = self.encoding.int_range()
            && limits.low_int < min
            && limits.high_int > max
        {
            return self;
        }
        Decoder {
            limits: Some(limits),
            ..self
        }
    }

    /// The limits at or beyond which values are missing, if any (see
    /// [`with_limits`](Self::with_limits)).
    pub fn limits(&self) -> Option<(f64, f64)> {
        self.limits.map(|l| (l.low, l.high))
    }

    /// Whether decoding subtracts a reference value: for the encodings that
    /// [use one](Encoding::uses_reference), and for int32 with limits.
    pub fn uses_reference(&self) -> bool {
        self.encoding.uses_reference() || (self.limits.is_some() && self.encoding == Encoding::I32)
    }

    /// The same decoder subtracting `reference` from every value, if it
    /// [uses one](Self::uses_reference); otherwise unchanged. A reference that is not finite
    /// counts as 0.
    pub fn with_reference(self, reference: f64) -> Self {
        let reference = if self.uses_reference() && reference.is_finite() {
            reference
        } else {
            0.0
        };
        Decoder { reference, ..self }
    }

    /// The reference to use for a channel whose first stored sample is `first`: its value,
    /// if the decoder uses a reference and the value is finite (not missing) and within
    /// float32's range; 0 otherwise. (Relative to a value beyond float32's range, every
    /// other value would read as infinite.) [`reference_among`](Self::reference_among)
    /// looks further.
    pub fn reference_from(&self, first: &[u8]) -> f64 {
        self.reference_among([first])
    }

    /// The reference to use for a channel whose first stored samples are `samples` (each
    /// slice starting at one): the first of up to [`REFERENCE_SEARCH`] of them whose value
    /// is finite and within float32's range, if the decoder uses a reference; 0 otherwise.
    /// Missing values (NaN) and values beyond float32's range, which read as infinite, are
    /// passed over, so a channel starting with them still keeps its variation.
    pub fn reference_among<'a>(&self, samples: impl IntoIterator<Item = &'a [u8]>) -> f64 {
        if !self.uses_reference() {
            return 0.0;
        }
        samples
            .into_iter()
            .take(REFERENCE_SEARCH)
            .filter_map(|bytes| self.value(bytes))
            .find(|v| v.abs() <= f64::from(f32::MAX))
            .unwrap_or(0.0)
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    pub fn order(&self) -> ByteOrder {
        self.order
    }

    /// The value subtracted from every sample; fold `reference × gain` into the offset.
    pub fn reference(&self) -> f64 {
        self.reference
    }

    pub fn native(&self) -> SampleType {
        if self.limits.is_some() {
            SampleType::F32
        } else {
            self.encoding.native()
        }
    }

    /// The stored value of the sample at the start of `bytes`, in f64 (rounded for 64-bit
    /// integers beyond 2^53), NaN for a missing one; `None` if `bytes` is too short.
    pub fn value(&self, bytes: &[u8]) -> Option<f64> {
        let value = self.stored(bytes)?;
        let missing = match self.limits {
            None => false,
            // Integers compare exactly: in f64, 64-bit ones beside a limit would round to it.
            Some(l) => match self.stored_int(bytes) {
                Some(v) => l.int_missing(v),
                None => l.float_missing(value),
            },
        };
        Some(if missing { f64::NAN } else { value })
    }

    /// The stored integer at the start of `bytes` (long enough), `None` for floats.
    fn stored_int(&self, bytes: &[u8]) -> Option<i128> {
        let n = self.encoding.size();
        let b = bytes.get(..n)?;
        let signed = self.encoding.int_range()?.0 < 0;
        // Little-endian from here on: the value is the sum of its bytes' weights.
        let ordered: [u8; 8] = std::array::from_fn(|i| match self.order {
            _ if i >= n => 0,
            ByteOrder::Little => b[i],
            ByteOrder::Big => b[n - 1 - i],
        });
        let unsigned = u64::from_le_bytes(ordered);
        let bits = 8 * n as u32;
        Some(if signed && unsigned >> (bits - 1) & 1 == 1 {
            i128::from(unsigned) - (1i128 << bits)
        } else {
            i128::from(unsigned)
        })
    }

    /// The stored value at the start of `bytes`, never NaN for integers.
    fn stored(&self, bytes: &[u8]) -> Option<f64> {
        let b = bytes.get(..self.encoding.size())?;
        let little = self.order == ByteOrder::Little;
        macro_rules! read {
            ($t:ty, $n:literal) => {{
                let a: [u8; $n] = b.try_into().ok()?;
                if little {
                    <$t>::from_le_bytes(a)
                } else {
                    <$t>::from_be_bytes(a)
                }
            }};
        }
        Some(match self.encoding {
            Encoding::I8 => f64::from(b[0] as i8),
            Encoding::U8 => f64::from(b[0]),
            Encoding::I16 => f64::from(read!(i16, 2)),
            Encoding::U16 => f64::from(read!(u16, 2)),
            Encoding::I24 => f64::from(i24(b.try_into().ok()?, little)),
            Encoding::U24 => f64::from(u24(b.try_into().ok()?, little)),
            Encoding::I32 => f64::from(read!(i32, 4)),
            Encoding::U32 => f64::from(read!(u32, 4)),
            Encoding::I64 => read!(i64, 8) as f64,
            Encoding::U64 => read!(u64, 8) as f64,
            Encoding::F32 => f64::from(read!(f32, 4)),
            Encoding::F64 => read!(f64, 8),
        })
    }

    /// Decodes `dst.len()` samples from `src`, one every `stride` bytes (the sample size
    /// for contiguous samples, the frame size for interleaved ones), starting at `src[0]`.
    ///
    /// Fails if `dst` is not of the native type or `src` is too short.
    pub fn decode<T: Sample>(
        &self,
        src: &[u8],
        stride: usize,
        dst: &mut [T],
    ) -> Result<(), ReadError> {
        let n = dst.len();
        if T::TYPE != self.native() {
            return Err(ReadError::BufferMismatch {
                expected: n,
                expected_type: self.native(),
                found: n,
                found_type: T::TYPE,
            });
        }
        let size = self.encoding.size();
        if n == 0 {
            return Ok(());
        }
        let needed = stride
            .checked_mul(n - 1)
            .and_then(|x| x.checked_add(size))
            .filter(|_| stride >= size);
        match needed {
            Some(needed) if needed <= src.len() => {}
            _ => {
                return Err(ReadError::Corrupt(format!(
                    "{n} samples of {size} bytes every {stride} bytes need more than the {} \
                     bytes there are",
                    src.len()
                )));
            }
        }
        // Exact length, so the loops below can zip without checking for a short last piece.
        let src = &src[..stride * (n - 1) + size];
        let little = self.order == ByteOrder::Little;
        let r = self.reference;
        // 64-bit integers subtract an integer reference exactly, then round once: in f64 they
        // would round before the subtraction (by up to 2^(e-53)). The reference is an integer
        // (a stored value), so the cast is exact.
        let ri = r as i128;
        match T::as_samples_mut(dst) {
            SamplesMut::I16(d) => match self.encoding {
                Encoding::I8 => each!(src, stride, d, 1, |[a]: [u8; 1]| i16::from(a as i8)),
                Encoding::U8 => each!(src, stride, d, 1, |[a]: [u8; 1]| i16::from(a)),
                Encoding::I16 if little => each!(src, stride, d, 2, i16::from_le_bytes),
                Encoding::I16 => each!(src, stride, d, 2, i16::from_be_bytes),
                _ => unreachable!("native type checked above"),
            },
            SamplesMut::I32(d) => match self.encoding {
                Encoding::U16 if little => {
                    each!(src, stride, d, 2, |a| i32::from(u16::from_le_bytes(a)))
                }
                Encoding::U16 => each!(src, stride, d, 2, |a| i32::from(u16::from_be_bytes(a))),
                Encoding::I24 if little => each!(src, stride, d, 3, |a| i24(a, true)),
                Encoding::I24 => each!(src, stride, d, 3, |a| i24(a, false)),
                Encoding::U24 if little => each!(src, stride, d, 3, |a| u24(a, true)),
                Encoding::U24 => each!(src, stride, d, 3, |a| u24(a, false)),
                Encoding::I32 if little => each!(src, stride, d, 4, i32::from_le_bytes),
                Encoding::I32 => each!(src, stride, d, 4, i32::from_be_bytes),
                _ => unreachable!("native type checked above"),
            },
            SamplesMut::F32(d) if let Some(l) = self.limits => {
                // Values at or beyond the limits become NaN; integers compare exactly.
                let int = move |v: i128, x: f32| if l.int_missing(v) { f32::NAN } else { x };
                let float = move |v: f64, x: f32| if l.float_missing(v) { f32::NAN } else { x };
                macro_rules! le_be {
                    ($n:literal, $t:ty, $f:expr) => {
                        if little {
                            each!(src, stride, d, $n, |a| $f(<$t>::from_le_bytes(a)))
                        } else {
                            each!(src, stride, d, $n, |a| $f(<$t>::from_be_bytes(a)))
                        }
                    };
                }
                match self.encoding {
                    Encoding::I8 => each!(src, stride, d, 1, |[a]: [u8; 1]| {
                        let v = a as i8;
                        int(v.into(), f32::from(v))
                    }),
                    Encoding::U8 => {
                        each!(src, stride, d, 1, |[a]: [u8; 1]| int(
                            a.into(),
                            f32::from(a)
                        ))
                    }
                    Encoding::I16 => le_be!(2, i16, |v: i16| int(v.into(), f32::from(v))),
                    Encoding::U16 => le_be!(2, u16, |v: u16| int(v.into(), f32::from(v))),
                    Encoding::I24 => each!(src, stride, d, 3, |a| {
                        let v = i24(a, little);
                        int(v.into(), v as f32)
                    }),
                    Encoding::U24 => each!(src, stride, d, 3, |a| {
                        let v = u24(a, little);
                        int(v.into(), v as f32)
                    }),
                    Encoding::I32 => le_be!(4, i32, |v: i32| {
                        int(v.into(), (f64::from(v) - r) as f32)
                    }),
                    Encoding::U32 => le_be!(4, u32, |v: u32| {
                        int(v.into(), (f64::from(v) - r) as f32)
                    }),
                    Encoding::I64 => le_be!(8, i64, |v: i64| {
                        int(v.into(), (i128::from(v) - ri) as f32)
                    }),
                    Encoding::U64 => le_be!(8, u64, |v: u64| {
                        int(v.into(), (i128::from(v) - ri) as f32)
                    }),
                    Encoding::F32 => le_be!(4, f32, |v: f32| float(v.into(), v)),
                    Encoding::F64 => le_be!(8, f64, |v: f64| float(v, (v - r) as f32)),
                }
            }
            SamplesMut::F32(d) => match self.encoding {
                Encoding::F32 if little => each!(src, stride, d, 4, f32::from_le_bytes),
                Encoding::F32 => each!(src, stride, d, 4, f32::from_be_bytes),
                Encoding::U32 if little => {
                    each!(
                        src,
                        stride,
                        d,
                        4,
                        |a| (f64::from(u32::from_le_bytes(a)) - r) as f32
                    )
                }
                Encoding::U32 => {
                    each!(
                        src,
                        stride,
                        d,
                        4,
                        |a| (f64::from(u32::from_be_bytes(a)) - r) as f32
                    )
                }
                Encoding::I64 if little => {
                    each!(
                        src,
                        stride,
                        d,
                        8,
                        |a| (i128::from(i64::from_le_bytes(a)) - ri) as f32
                    )
                }
                Encoding::I64 => {
                    each!(
                        src,
                        stride,
                        d,
                        8,
                        |a| (i128::from(i64::from_be_bytes(a)) - ri) as f32
                    )
                }
                Encoding::U64 if little => {
                    each!(
                        src,
                        stride,
                        d,
                        8,
                        |a| (i128::from(u64::from_le_bytes(a)) - ri) as f32
                    )
                }
                Encoding::U64 => {
                    each!(
                        src,
                        stride,
                        d,
                        8,
                        |a| (i128::from(u64::from_be_bytes(a)) - ri) as f32
                    )
                }
                Encoding::F64 if little => {
                    each!(src, stride, d, 8, |a| (f64::from_le_bytes(a) - r) as f32)
                }
                Encoding::F64 => each!(src, stride, d, 8, |a| (f64::from_be_bytes(a) - r) as f32),
                _ => unreachable!("native type checked above"),
            },
        }
        Ok(())
    }
}

/// A 24-bit two's-complement integer, sign-extended by the arithmetic shift.
#[inline(always)]
fn i24(b: [u8; 3], little: bool) -> i32 {
    if little {
        i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8
    } else {
        i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8
    }
}

#[inline(always)]
fn u24(b: [u8; 3], little: bool) -> i32 {
    if little {
        i32::from_le_bytes([b[0], b[1], b[2], 0])
    } else {
        i32::from_be_bytes([0, b[0], b[1], b[2]])
    }
}

/// Bytes of sample-major data decoded per pass over the requested channels, so a pass's
/// frames stay in the CPU cache while each channel is picked out of them.
const PIECE_BYTES: usize = 256 << 10;

/// Decodes `range` of sample-major data (frames of `frame` bytes, one sample of every
/// channel each, starting at `data[0]`) into `dst`, channel-major: one `column` per
/// requested channel, its byte offset within a frame and its decoder.
pub(crate) fn frames<T: Sample>(
    data: &[u8],
    frame: usize,
    columns: &[(usize, Decoder)],
    range: std::ops::Range<u64>,
    dst: &mut [T],
) -> Result<(), ReadError> {
    let n = (range.end - range.start) as usize;
    frames_into(data, frame, columns, range, dst, n, 0)
}

/// As [`frames`], into part of a larger channel-major `dst`: channel `k`'s samples go to
/// `dst[k * stride + at..]`, so pieces of one read can come from different places (FIFF's
/// data buffers).
pub(crate) fn frames_into<T: Sample>(
    data: &[u8],
    frame: usize,
    columns: &[(usize, Decoder)],
    range: std::ops::Range<u64>,
    dst: &mut [T],
    stride: usize,
    at: usize,
) -> Result<(), ReadError> {
    let n = (range.end - range.start) as usize;
    if n == 0 || frame == 0 {
        return Ok(());
    }
    let outside = || ReadError::Corrupt("samples beyond the end of the data".into());
    let piece = (PIECE_BYTES / frame).max(1);
    let mut done = 0;
    while done < n {
        let take = piece.min(n - done);
        let start = (range.start as usize + done) * frame;
        let bytes = data.get(start..start + take * frame).ok_or_else(outside)?;
        for (k, (column, decoder)) in columns.iter().enumerate() {
            let samples = dst
                .get_mut(k * stride + at + done..)
                .and_then(|d| d.get_mut(..take))
                .ok_or_else(|| ReadError::Corrupt("output buffer too short".into()))?;
            decoder.decode(bytes.get(*column..).ok_or_else(outside)?, frame, samples)?;
        }
        done += take;
    }
    Ok(())
}
