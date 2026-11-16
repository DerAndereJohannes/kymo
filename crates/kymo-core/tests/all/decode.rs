//! Decoding stored samples into native types, against a plain conversion of each value.

use kymo_core::decode::{ByteOrder, Decoder, Encoding, REFERENCE_SEARCH, strided};
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

/// Whether the stored value of `bytes` is missing under the limits `(low, high)`: at or
/// beyond them, integers compared exactly (by hand, as `stored` decodes them).
fn beyond(encoding: Encoding, order: ByteOrder, bytes: &[u8], (low, high): (f64, f64)) -> bool {
    let value = stored(encoding, order, bytes);
    if matches!(encoding, Encoding::F32 | Encoding::F64) {
        return value <= low || value >= high;
    }
    let mut b = bytes[..encoding.size()].to_vec();
    if order == ByteOrder::Big {
        b.reverse();
    }
    let unsigned = b
        .iter()
        .rev()
        .fold(0i128, |acc, &byte| (acc << 8) | i128::from(byte));
    let bits = 8 * encoding.size() as u32;
    let signed = matches!(
        encoding,
        Encoding::I8 | Encoding::I16 | Encoding::I24 | Encoding::I32 | Encoding::I64
    );
    let v = if signed && unsigned >> (bits - 1) & 1 == 1 {
        unsigned - (1i128 << bits)
    } else {
        unsigned
    };
    v <= low.floor() as i128 || v >= high.ceil() as i128
}

/// Limits for `encoding` from two fractions in -1..1: around the integer types' range (a
/// little beyond it, so some leave every value in between), over many magnitudes for floats.
fn limits_of(encoding: Encoding, (a, b): (f64, f64)) -> (f64, f64) {
    let f = |x: f64| match encoding.int_range() {
        Some((min, max)) => (min as f64 + (x + 1.0) / 2.0 * (max - min) as f64 * 1.1).round(),
        None => x.signum() * 10f64.powf(x.abs() * 40.0 - 10.0),
    };
    (f(a).min(f(b)), f(a).max(f(b)))
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
        fractions in proptest::option::of((-1.0..1.0f64, -1.0..1.0f64)),
    ) {
        let size = encoding.size();
        let stride = size + padding;
        let limits = fractions.map(|f| limits_of(encoding, f)).filter(|(low, high)| low < high);
        let decoder = Decoder::new(encoding, order);
        let decoder = match limits {
            Some((low, high)) => decoder.with_limits(low, high),
            None => decoder,
        };
        let decoder = decoder.with_reference(reference);
        // Limits that leave every integer of the encoding in between change nothing.
        let limits = limits.filter(|&(low, high)| match encoding.int_range() {
            Some((min, max)) => low.floor() as i128 >= min || high.ceil() as i128 <= max,
            None => true,
        });
        prop_assert_eq!(decoder.limits(), limits);
        let uses = encoding.uses_reference() || (limits.is_some() && encoding == Encoding::I32);
        prop_assert_eq!(decoder.uses_reference(), uses);
        let used = if uses && reference.is_finite() { reference } else { 0.0 };
        prop_assert_eq!(decoder.reference(), used);
        // Exactly as many bytes as the samples need: no padding after the last one.
        let src = &bytes[..if n == 0 { 0 } else { stride * (n - 1) + size }];
        let got = decoded(&decoder, src, stride, n).unwrap();
        for (i, &g) in got.iter().enumerate() {
            let bytes = &src[i * stride..];
            let value = stored(encoding, order, bytes);
            let gone = limits.is_some_and(|l| beyond(encoding, order, bytes, l));
            let value = if gone { f64::NAN } else { value };
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

/// Values at or beyond the limits read as missing, those between them as they are, in
/// every encoding; limits that leave every value of an integer encoding in between, or
/// aren't finite with low < high, change nothing.
#[test]
fn values_at_or_beyond_the_limits_are_missing() {
    let values: [f64; 8] = [-5.0, 10.0, 10.5, 11.0, 19.0, 19.9, 20.0, 25.0];
    for encoding in Encoding::ALL {
        let signed = encoding.int_range().is_none_or(|(min, _)| min < 0);
        let float = encoding.int_range().is_none();
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let put = |v: f64| -> Vec<u8> {
                let mut b: Vec<u8> = match encoding {
                    Encoding::F32 => (v as f32).to_le_bytes().to_vec(),
                    Encoding::F64 => v.to_le_bytes().to_vec(),
                    _ => (v as i64).to_le_bytes()[..encoding.size()].to_vec(),
                };
                if order == ByteOrder::Big {
                    b.reverse();
                }
                b
            };
            let kept: Vec<f64> = values
                .into_iter()
                .filter(|&v| (signed || v >= 0.0) && (float || v.fract() == 0.0))
                .collect();
            let bytes: Vec<u8> = kept.iter().flat_map(|&v| put(v)).collect();
            let d = Decoder::new(encoding, order).with_limits(10.5, 20.0);
            assert_eq!(d.native(), SampleType::F32, "{encoding:?}");
            let d = d.with_reference(d.reference_among(strided(&bytes, encoding.size(), 9)));
            let mut out = vec![0f32; kept.len()];
            d.decode(&bytes, encoding.size(), &mut out).unwrap();
            for (k, (&v, &got)) in kept.iter().zip(&out).enumerate() {
                let missing = !(10.5 < v && v < 20.0);
                let at = &bytes[k * encoding.size()..];
                assert_eq!(d.value(at).unwrap().is_nan(), missing, "{encoding:?} {v}");
                assert_eq!(got.is_nan(), missing, "{encoding:?} {order:?} {v}");
                if !missing {
                    // The stored value (as float32 stores it) minus the reference, rounded
                    // once to float32.
                    let v = if encoding == Encoding::F32 {
                        f64::from(v as f32)
                    } else {
                        v
                    };
                    assert_eq!(got, (v - d.reference()) as f32, "{encoding:?} {v}");
                }
            }
            // Interleaved too.
            let spaced: Vec<u8> = kept
                .iter()
                .flat_map(|&v| [put(v), vec![7, 7]].concat())
                .collect();
            let mut again = vec![0f32; kept.len()];
            d.decode(&spaced, encoding.size() + 2, &mut again).unwrap();
            assert!(
                out.iter()
                    .zip(&again)
                    .all(|(a, b)| same((*a).into(), (*b).into()))
            );
        }
        for (low, high) in [
            (f64::NAN, 1.0),
            (5.0, 5.0),
            (6.0, 5.0),
            (0.0, f64::INFINITY),
        ] {
            let d = Decoder::new(encoding, ByteOrder::Little).with_limits(low, high);
            assert_eq!(d.limits(), None, "{encoding:?} {low} {high}");
            assert_eq!(d.native(), encoding.native());
        }
        if let Some((min, max)) = encoding.int_range() {
            let wide = Decoder::new(encoding, ByteOrder::Little)
                .with_limits(min as f64 * 1.01 - 1.0, max as f64 * 1.01 + 1.0);
            assert_eq!(wide.limits(), None, "{encoding:?}");
            // The type's own extremes as limits: those values are missing.
            let full =
                Decoder::new(encoding, ByteOrder::Little).with_limits(min as f64, max as f64);
            assert!(full.limits().is_some(), "{encoding:?}");
        }
    }
}

#[test]
fn the_reference_is_the_first_finite_value() {
    let f64s = Decoder::new(Encoding::F64, ByteOrder::Big);
    assert_eq!(f64s.reference_from(&1234.5f64.to_be_bytes()), 1234.5);
    assert_eq!(f64s.reference_from(&f64::NAN.to_be_bytes()), 0.0);
    // Relative to a value beyond float32's range, every other value would be infinite.
    assert_eq!(f64s.reference_from(&1.3e212f64.to_be_bytes()), 0.0);
    assert_eq!(f64s.reference_from(&3e38f64.to_be_bytes()), 3e38);
    assert_eq!(f64s.reference_from(&[0, 1]), 0.0);
    let u32s = Decoder::new(Encoding::U32, ByteOrder::Little);
    assert_eq!(u32s.reference_from(&4_000_000_000u32.to_le_bytes()), 4e9);
    // Further samples: the first finite one within float32's range, within the first 4096.
    let bytes: Vec<[u8; 8]> = [f64::NAN, 1.3e212, f64::INFINITY, 7.5, 9.0]
        .map(f64::to_be_bytes)
        .to_vec();
    assert_eq!(f64s.reference_among(bytes.iter().map(|b| &b[..])), 7.5);
    let all = [f64::NAN.to_be_bytes(); 2];
    assert_eq!(f64s.reference_among(all.iter().map(|b| &b[..])), 0.0);
    let late: Vec<[u8; 8]> = (0..=REFERENCE_SEARCH)
        .map(|i| if i < REFERENCE_SEARCH { f64::NAN } else { 1.0 }.to_be_bytes())
        .collect();
    assert_eq!(f64s.reference_among(late.iter().map(|b| &b[..])), 0.0);
    // A missing int32 (beyond GDF's digital limits) is passed over too.
    let i32s = Decoder::new(Encoding::I32, ByteOrder::Little).with_limits(-2e9, 2e9);
    let bytes = [i32::MIN, 40_000_000].map(i32::to_le_bytes).concat();
    assert_eq!(i32s.reference_among(strided(&bytes, 4, 2)), 4e7);
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

/// 64-bit integers beyond 2^53 keep their difference to the reference exact before it is
/// rounded to f32: a nanosecond clock near 2^60 ticks in steps of 1, not of f64's 256.
#[test]
fn int64_differences_round_once() {
    let base = 1i64 << 60;
    for (encoding, values) in [
        (
            Encoding::I64,
            [base + 1, base + 3, base + 300].map(|v| v.to_le_bytes()),
        ),
        (
            Encoding::U64,
            [base + 1, base + 3, base + 300].map(|v| (v as u64).to_le_bytes()),
        ),
    ] {
        let d = Decoder::new(encoding, ByteOrder::Little);
        let bytes = values.concat();
        let d = d.with_reference(d.reference_from(&bytes));
        let mut out = [0f32; 3];
        d.decode(&bytes, 8, &mut out).unwrap();
        // Differences to the first value, each exact in f32.
        let first = out[0];
        assert_eq!(
            [out[1] - first, out[2] - first],
            [2.0, 299.0],
            "{encoding:?}"
        );
    }
    // At GDF's digital minimum i64::MIN, exactly i64::MIN is missing; its neighbor isn't.
    let d = Decoder::new(Encoding::I64, ByteOrder::Little)
        .with_limits(i64::MIN as f64, i64::MAX as f64);
    assert_eq!(
        d.value(&(i64::MIN + 1).to_le_bytes()),
        Some(i64::MIN as f64)
    );
}
