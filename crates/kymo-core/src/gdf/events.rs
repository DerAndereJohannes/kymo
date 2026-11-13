//! GDF's event table, after the data records: modes 1 (position and type) and 3 (also
//! channel and duration), as annotations.

use crate::{AnnotationData, ChannelId, Origin};

/// The type code of sparse samples (a value of a channel without samples in the records,
/// stored in the duration field), which are not events.
const SPARSE_SAMPLE: u16 = 0x7fff;

/// The event table's header form changed with GDF 1.94: before, 3 bytes of event rate and
/// 4 of count; from then on, 3 bytes of count and a float32 rate.
const NEW_TABLE_HEADER: f64 = 1.94;

/// Reads the event table at the start of `bytes` (to the end of the file).
///
/// Onsets are (position − 1) / event rate, with the highest grid rate (`fallback_rate`)
/// for a rate of 0; durations of mode 3 likewise, mode 1 events are points in time. The
/// description is the type code in decimal, as MNE gives it. A mode 3 channel number counts
/// the file's channels from 1, 0 meaning all.
pub(super) fn read(
    bytes: &[u8],
    version: f64,
    fallback_rate: f64,
    file_channels: &[Option<ChannelId>],
    warnings: &mut Vec<String>,
) -> Vec<AnnotationData> {
    let Some(header) = bytes.first_chunk::<8>() else {
        warnings.push(format!(
            "the event table is truncated ({} bytes); no events are read",
            bytes.len()
        ));
        return Vec::new();
    };
    let mode = header[0];
    let u24 = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], 0]);
    let (n, rate) = if version < NEW_TABLE_HEADER {
        let n = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        (n as usize, f64::from(u24(&header[1..4])))
    } else {
        let rate = f32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        (u24(&header[1..4]) as usize, f64::from(rate))
    };
    if mode != 1 && mode != 3 {
        warnings.push(format!(
            "event table mode {mode} is not supported; no events are read"
        ));
        return Vec::new();
    }
    let rate = if rate.is_finite() && rate > 0.0 {
        rate
    } else {
        fallback_rate
    };
    // Each field holds all events in turn: positions (4 bytes), types (2), and for mode 3
    // channels (2) and durations (4). An event is there if its last field is.
    let table = &bytes[8..];
    let (pos_at, typ_at, chn_at, dur_at) = (0, 4 * n, 6 * n, 8 * n);
    let (last_at, last_width) = if mode == 3 { (dur_at, 4) } else { (typ_at, 2) };
    let present = table
        .len()
        .checked_sub(last_at)
        .map_or(0, |rest| (rest / last_width).min(n));
    if present < n {
        warnings.push(format!(
            "the event table is truncated: it lists {n} events, the file holds {present}"
        ));
    }
    let field = |offset: usize, width: usize, k: usize| -> &[u8] {
        let at = offset + width * k;
        table.get(at..at + width).unwrap_or(&[0; 4][..width])
    };

    let mut events = Vec::with_capacity(present);
    let (mut before_start, mut unknown_channels) = (0usize, 0usize);
    for k in 0..present {
        let le32 = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let position = le32(field(pos_at, 4, k));
        let typ = u16::from_le_bytes([field(typ_at, 2, k)[0], field(typ_at, 2, k)[1]]);
        if typ == SPARSE_SAMPLE {
            continue;
        }
        if position == 0 {
            before_start += 1;
            continue;
        }
        let (channels, duration) = if mode == 3 {
            let c = field(chn_at, 2, k);
            let channel = u16::from_le_bytes([c[0], c[1]]);
            let duration = f64::from(le32(field(dur_at, 4, k))) / rate;
            let channels = match channel {
                0 => Vec::new(),
                c => match file_channels.get(usize::from(c) - 1).copied().flatten() {
                    Some(id) => vec![id],
                    None => {
                        unknown_channels += 1;
                        Vec::new()
                    }
                },
            };
            (channels, duration)
        } else {
            (Vec::new(), 0.0)
        };
        events.push(AnnotationData {
            onset: f64::from(position - 1) / rate,
            duration,
            description: typ.to_string(),
            channels,
            origin: Origin::File,
            stream: None,
        });
    }
    if before_start > 0 {
        warnings.push(format!(
            "{before_start} events at position 0, before the first sample, are left out"
        ));
    }
    if unknown_channels > 0 {
        warnings.push(format!(
            "{unknown_channels} events name a channel the file doesn't have, or one without \
             samples; they apply to all channels"
        ));
    }
    events
}
