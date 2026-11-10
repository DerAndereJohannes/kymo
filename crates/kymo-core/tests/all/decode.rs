//! Decoding stored samples into native types, against a plain conversion of each value.

use kymo_core::decode::{ByteOrder, Decoder, Encoding};
use kymo_core::{ReadError, Sample, SampleType};
use proptest::prelude::*;

/// The stored value of `bytes` in f64, decoded by hand: the reference the decoder is checked
/// against. Exact except for 64-bit integers beyond 2^53, rounded as `as f64` rounds.
fn stored(encoding: Encoding, order: ByteOrder, bytes: &[u8]) -> f64 {
    let mut b = bytes[..encoding.size()].to_vec();
    if order == ByteOrder::Big {
        b.reverse();
    }
    // Little-endian from here on: the value is the sum of its bytes' weights.
    let unsigned = b
        .iter()
        .rev()
        .fold(0u128, |acc, &byte| (acc << 8) | u128::from(byte));
    let bits = 8 * encoding.size() as u32;
    let signed = || {
        let sign = 1u128 << (bits - 1);
        if unsigned & sign != 0 {
            unsigned as i128 - (1i128 << bits)
        } else {
            unsigned as i128
        }
    };
    match encoding {
        Encoding::U8 | Encoding::U16 | Encoding::U24 | Encoding::U32 => unsigned as f64,
        Encoding::U64 => (unsigned as u64) as f64,
        Encoding::I8 | Encoding::I16 | Encoding::I24 | Encoding::I32 => signed() as f64,
        Encoding::I64 => (signed() as i64) as f64,
        Encoding::F32 => f64::from(f32::from_bits(unsigned as u32)),
        Encoding::F64 => f64::from_bits(unsigned as u64),
    }
}

fn encodings() -> impl Strategy<Value = Encoding> {
    proptest::sample::select(Encoding::ALL.to_vec())
}

fn orders() -> impl Strategy<Value = ByteOrder> {
    prop_oneof![Just(ByteOrder::Little), Just(ByteOrder::Big)]
}

/// Decodes `n` samples every `stride` bytes into the native type, as f64.
fn decoded(decoder: &Decoder, src: &[u8], stride: usize, n: usize) -> Result<Vec<f64>, ReadError> {
    fn run<T: Sample>(
        d: &Decoder,
        src: &[u8],
        stride: usize,
        n: usize,
    ) -> Result<Vec<f64>, ReadError> {
        let mut out = vec![T::default(); n];
        d.decode(src, stride, &mut out)?;
        Ok(out.into_iter().map(Sample::to_f64).collect())
    }
    match decoder.native() {
        SampleType::I16 => run::<i16>(decoder, src, stride, n),
        SampleType::I32 => run::<i32>(decoder, src, stride, n),
        SampleType::F32 => run::<f32>(decoder, src, stride, n),
        other => panic!("unexpected native type {other:?}"),
    }
}

/// Two values are the same, NaN included.
fn same(a: f64, b: f64) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

proptest! {
    /// Every sample decodes to its stored value: exactly for integer native types, and as
    /// `value − reference` rounded to f32 otherwise. Contiguous and interleaved samples alike.
    #[test]
    fn samples_decode_to_their_stored_values(
        encoding in encodings(),
        order in orders(),
        n in 0..40usize,
        padding in 0..6usize,
        bytes in proptest::collection::vec(any::<u8>(), 40 * 14),
        reference in prop_oneof![Just(0.0), -1e9..1e9f64, Just(f64::NAN)],
        nan_minimum in any::<bool>(),
    ) {
        let size = encoding.size();
        let stride = size + padding;
        let decoder = Decoder::new(encoding, order);
        let decoder = if nan_minimum { decoder.with_nan_minimum() } else { decoder };
        let decoder = decoder.with_reference(reference);
        let signed = matches!(encoding, Encoding::I8 | Encoding::I16 | Encoding::I24 | Encoding::I32 | Encoding::I64);
        let missing = nan_minimum && signed;
        prop_assert_eq!(decoder.nan_minimum(), missing);
        let uses = encoding.uses_reference() || (missing && encoding == Encoding::I32);
        prop_assert_eq!(decoder.uses_reference(), uses);
        let used = if uses && reference.is_finite() { reference } else { 0.0 };
        prop_assert_eq!(decoder.reference(), used);
        // Exactly as many bytes as the samples need: no padding after the last one.
        let src = &bytes[..if n == 0 { 0 } else { stride * (n - 1) + size }];
        let got = decoded(&decoder, src, stride, n).unwrap();
        for (i, &g) in got.iter().enumerate() {
            let value = stored(encoding, order, &src[i * stride..]);
            let minimum = -(2f64.powi(8 * size as i32 - 1));
            let value = if missing && value == minimum { f64::NAN } else { value };
            prop_assert!(same(decoder.value(&src[i * stride..]).unwrap(), value));
            let want = match decoder.native() {
                SampleType::F32 if uses => f64::from((value - used) as f32),
                SampleType::F32 => f64::from(value as f32),
                _ => value,
            };
            prop_assert!(same(g, want), "{:?} {:?} sample {}: {} != {}", encoding, order, i, g, want);
            if decoder.native() != SampleType::F32 {
                // Integers decode exactly, so native + reference is the stored value.
                prop_assert_eq!(g + decoder.reference(), value);
            }
        }
    }

    /// Too few bytes, a stride shorter than a sample or a buffer of another type fail, and
    /// never panic.
    #[test]
    fn bad_requests_are_errors(
        encoding in encodings(),
        order in orders(),
        n in 1..20usize,
        short_by in 1..4usize,
    ) {
        let size = encoding.size();
        let decoder = Decoder::new(encoding, order);
        let bytes = vec![0u8; size * n];
        let short = &bytes[..(size * n).saturating_sub(short_by)];
        prop_assert!(decoded(&decoder, short, size, n).is_err());
        if size > 1 {
            prop_assert!(decoded(&decoder, &bytes, size - 1, n).is_err());
        }
        let result = match decoder.native() {
            SampleType::I16 => decoder.decode(&bytes, size, &mut vec![0i32; n]),
            _ => decoder.decode(&bytes, size, &mut vec![0i16; n]),
        };
        let mismatch = matches!(result, Err(ReadError::BufferMismatch { .. }));
        prop_assert!(mismatch);
    }
}

#[test]
fn native_types_and_references() {
    use Encoding::*;
    let i16s = [I8, U8, I16];
    let i32s = [U16, I24, U24, I32];
    for e in Encoding::ALL {
        let native = if i16s.contains(&e) {
            SampleType::I16
        } else if i32s.contains(&e) {
            SampleType::I32
        } else {
            SampleType::F32
        };
        assert_eq!(e.native(), native, "{e:?}");
        assert_eq!(
            e.uses_reference(),
            [U32, I64, U64, F64].contains(&e),
            "{e:?}"
        );
    }
}

/// Random bytes rarely hold a type's smallest value, so each signed encoding gets it here:
/// with a NaN minimum it reads as NaN, its neighbours as they are; without, as a number.
#[test]
fn the_smallest_signed_value_can_be_missing() {
    for encoding in [
        Encoding::I8,
        Encoding::I16,
        Encoding::I24,
        Encoding::I32,
        Encoding::I64,
    ] {
        let size = encoding.size();
        for order in [ByteOrder::Little, ByteOrder::Big] {
            // The minimum is 0x80 followed by zeros (big-endian), then the minimum + 1.
            let mut min = vec![0u8; size];
            min[0] = 0x80;
            let mut next = min.clone();
            next[size - 1] |= 1;
            if order == ByteOrder::Little {
                min.reverse();
                next.reverse();
            }
            let bytes = [min, next].concat();
            let minimum = -(2f64.powi(8 * size as i32 - 1));
            let plain = Decoder::new(encoding, order);
            let missing = plain.with_nan_minimum().with_reference(5.0);
            assert_eq!(missing.native(), SampleType::F32, "{encoding:?}");
            assert_eq!(plain.value(&bytes), Some(minimum), "{encoding:?} {order:?}");
            assert!(
                missing.value(&bytes).unwrap().is_nan(),
                "{encoding:?} {order:?}"
            );
            assert_eq!(missing.reference_from(&bytes), 0.0, "{encoding:?}");
            let mut out = [0f32; 2];
            missing.decode(&bytes, size, &mut out).unwrap();
            assert!(out[0].is_nan(), "{encoding:?} {order:?}");
            let r = missing.reference();
            assert_eq!(out[1], (minimum + 1.0 - r) as f32, "{encoding:?} {order:?}");
            // Interleaved too.
            let spaced = [&bytes[..size], &[7, 7], &bytes[size..]].concat();
            missing.decode(&spaced, size + 2, &mut out).unwrap();
            assert!(out[0].is_nan() && out[1] == (minimum + 1.0 - r) as f32);
        }
    }
    for encoding in [
        Encoding::U8,
        Encoding::U16,
        Encoding::U32,
        Encoding::F32,
        Encoding::F64,
    ] {
        let d = Decoder::new(encoding, ByteOrder::Little).with_nan_minimum();
        assert!(!d.nan_minimum(), "{encoding:?}");
        assert_eq!(d.native(), encoding.native());
    }
}

#[test]
fn the_reference_is_the_first_finite_value() {
    let f64s = Decoder::new(Encoding::F64, ByteOrder::Big);
    assert_eq!(f64s.reference_from(&1234.5f64.to_be_bytes()), 1234.5);
    assert_eq!(f64s.reference_from(&f64::NAN.to_be_bytes()), 0.0);
    assert_eq!(f64s.reference_from(&[0, 1]), 0.0);
    let u32s = Decoder::new(Encoding::U32, ByteOrder::Little);
    assert_eq!(u32s.reference_from(&4_000_000_000u32.to_le_bytes()), 4e9);
    // Exact encodings never subtract one.
    let i16s = Decoder::new(Encoding::I16, ByteOrder::Little);
    assert_eq!(i16s.reference_from(&1000i16.to_le_bytes()), 0.0);
    assert_eq!(i16s.with_reference(1000.0).reference(), 0.0);
}

/// Values far from zero keep their variation at f32 precision relative to the reference,
/// where a plain f32 would lose it.
#[test]
fn a_reference_keeps_precision_far_from_zero() {
    let base = 1.0e9f64;
    let values = [base, base + 0.25, base - 0.5, base + 3.75];
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let decoder = Decoder::new(Encoding::F64, ByteOrder::Little);
    let decoder = decoder.with_reference(decoder.reference_from(&bytes));
    let mut out = [0f32; 4];
    decoder.decode(&bytes, 8, &mut out).unwrap();
    for (got, want) in out.iter().zip(values) {
        assert_eq!(f64::from(*got) + decoder.reference(), want);
    }
    assert_ne!(f64::from(base as f32 + 0.25), base + 0.25);
}
