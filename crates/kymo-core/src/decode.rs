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
//! | uint32, int64, uint64, float64 | `F32` | `value − reference`, rounded to f32 |
//!
//! The reference of the last row is a per-channel value the reader picks (the channel's
//! first sample, see [`Decoder::reference_from`]) and folds into the channel's offset:
//! `physical = (native + reference) × gain + offset`, so the offset becomes
//! `offset + reference × gain`. A recording whose values sit far from zero then keeps their
//! variation at full f32 precision, as the renderer keeps EDF/BDF's huge offsets. Stim
//! channels keep a reference of 0: their native values are the codes.

use crate::{ReadError, Sample, SampleType, SamplesMut};

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

/// Decodes samples of one encoding and byte order into their native type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decoder {
    encoding: Encoding,
    order: ByteOrder,
    reference: f64,
}

impl Decoder {
    /// A decoder with a reference of 0.
    pub fn new(encoding: Encoding, order: ByteOrder) -> Self {
        Decoder {
            encoding,
            order,
            reference: 0.0,
        }
    }

    /// The same decoder subtracting `reference` from every value, if its encoding
    /// [uses one](Encoding::uses_reference); otherwise unchanged. A reference that is not
    /// finite counts as 0.
    pub fn with_reference(self, reference: f64) -> Self {
        let reference = if self.encoding.uses_reference() && reference.is_finite() {
            reference
        } else {
            0.0
        };
        Decoder { reference, ..self }
    }

    /// The reference to use for a channel whose first stored sample is `first`: its value,
    /// if the encoding uses a reference and the value is finite; 0 otherwise.
    pub fn reference_from(&self, first: &[u8]) -> f64 {
        match self.value(first) {
            Some(v) if self.encoding.uses_reference() && v.is_finite() => v,
            _ => 0.0,
        }
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
        self.encoding.native()
    }

    /// The stored value of the sample at the start of `bytes`, in f64 (rounded for 64-bit
    /// integers beyond 2^53); `None` if `bytes` is too short.
    pub fn value(&self, bytes: &[u8]) -> Option<f64> {
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
                    each!(src, stride, d, 8, |a| (i64::from_le_bytes(a) as f64 - r)
                        as f32)
                }
                Encoding::I64 => {
                    each!(src, stride, d, 8, |a| (i64::from_be_bytes(a) as f64 - r)
                        as f32)
                }
                Encoding::U64 if little => {
                    each!(src, stride, d, 8, |a| (u64::from_le_bytes(a) as f64 - r)
                        as f32)
                }
                Encoding::U64 => {
                    each!(src, stride, d, 8, |a| (u64::from_be_bytes(a) as f64 - r)
                        as f32)
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
