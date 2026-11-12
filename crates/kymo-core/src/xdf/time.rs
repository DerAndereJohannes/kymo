//! Time stamps of XDF streams, as pyxdf's `load_xdf` makes them by default:
//!
//! - **Clock synchronization:** a stream's ClockOffset chunks pair a time of its own clock
//!   with the offset that brings it to the recording's clock. The offsets are fitted with a
//!   line per range between clock resets (a robust Huber fit), and every time stamp gets the
//!   offset of its range's line at its time.
//! - **Dejittering:** a regular stream's synchronized time stamps are cut at breaks (steps
//!   of more than 1 s and 2 sample periods, either way), and each segment's are replaced by
//!   their least-squares line. pyxdf's breaks are steps of more than 1 s and 500 periods,
//!   which keeps steps of up to 7.8 s at 64 Hz inside a segment; but dejittering places a
//!   segment's samples as if none were missing, so samples after a step of lost samples
//!   land up to its length early (a Muse headband's stray samples 4 s early).
//!
//! Nothing here holds a stream's time stamps: they are fed in one by one, in order.

use crate::grid::Piece;

/// Offsets and clock times are fitted divided by this (seconds), so the Huber loss treats
/// deviations beyond it robustly (pyxdf's `winsor_threshold`).
const WINSOR: f64 = 1e-4;
/// Iterations of the robust fit (pyxdf's).
const FIT_ITERATIONS: usize = 1000;
/// A clock reset needs a step in clock times this many MADs and seconds from the median
/// (pyxdf's `clock_reset_threshold_stds` and `_seconds`)...
const RESET_TIME: (f64, f64) = (5.0, 5.0);
/// ...and a step in offsets this many MADs and seconds from the median (`_offset_stds`,
/// `_offset_seconds`). Clock times that run backwards are always a reset.
const RESET_OFFSET: (f64, f64) = (10.0, 1.0);
/// Breaks in a regular stream: steps longer than this many seconds and sample periods
/// (pyxdf's `jitter_break_threshold_seconds` and `_samples`, which default to 1 and 500).
const BREAK: (f64, f64) = (1.0, 2.0);
/// Longer gaps between segments are taken for damaged time stamps: the segment after one
/// follows the previous directly.
pub(crate) const MAX_GAP_SECONDS: f64 = 30.0 * 86_400.0;

/// One range of a stream's clock between resets.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ClockRange {
    /// The offset's line: `offset(t) = intercept + slope · t`.
    intercept: f64,
    slope: f64,
    /// The clock times of its first and last ClockOffset chunk.
    first: f64,
    last: f64,
}

/// The correction of a stream's clock.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Clock {
    ranges: Vec<ClockRange>,
}

impl Clock {
    /// The correction from a stream's ClockOffset chunks, (clock time, offset) in file
    /// order; `None` without any. Ranges whose offsets can't be fitted (all at one time)
    /// get no correction, with a warning.
    pub fn new(offsets: &[(f64, f64)], name: &str, warnings: &mut Vec<String>) -> Option<Clock> {
        let finite: Vec<(f64, f64)> = offsets
            .iter()
            .copied()
            .filter(|(t, o)| t.is_finite() && o.is_finite())
            .collect();
        if finite.len() < offsets.len() {
            warnings.push(format!(
                "{name}: {} clock offsets that aren't numbers are ignored",
                offsets.len() - finite.len()
            ));
        }
        let offsets = finite.as_slice();
        if offsets.is_empty() {
            return None;
        }
        let ranges = if offsets.len() > 1 {
            resets(offsets)
        } else {
            vec![(0, 0)]
        };
        let ranges = ranges
            .into_iter()
            .map(|(a, b)| {
                let part = &offsets[a..=b];
                let (intercept, slope) = if a == b {
                    (part[0].1, 0.0)
                } else {
                    robust_line(part).unwrap_or_else(|| {
                        warnings.push(format!(
                            "{name}: clock offsets {a} to {b} can't be fitted; they are not \
                             applied"
                        ));
                        (0.0, 0.0)
                    })
                };
                ClockRange {
                    intercept,
                    slope,
                    first: part[0].0,
                    last: part[part.len() - 1].0,
                }
            })
            .collect();
        Some(Clock { ranges })
    }

    /// How many times the clock was reset.
    pub fn resets(&self) -> usize {
        self.ranges.len() - 1
    }
}

/// Corrects a stream's time stamps, fed in order: each belongs to the range it is in, up to
/// the first that is closer to the next range's first clock time than to the end of its own
/// (pyxdf's rule, which assumes time stamps that increase).
pub(crate) struct Corrector<'a> {
    clock: Option<&'a Clock>,
    range: usize,
}

impl<'a> Corrector<'a> {
    pub fn new(clock: Option<&'a Clock>) -> Self {
        Corrector { clock, range: 0 }
    }

    pub fn correct(&mut self, t: f64) -> f64 {
        let Some(clock) = self.clock else { return t };
        let ranges = &clock.ranges;
        while self.range + 1 < ranges.len() {
            let (end, next) = (ranges[self.range].last, ranges[self.range + 1].first);
            if (t - end).abs() < (t - next).abs() {
                break;
            }
            self.range += 1;
        }
        let r = ranges[self.range];
        t + (r.intercept + r.slope * t)
    }
}

/// The ranges (first and last offset, inclusive) between clock resets: where clock times
/// run backwards, or where both the step in clock times and the step in offsets lie far
/// from their median.
fn resets(offsets: &[(f64, f64)]) -> Vec<(usize, usize)> {
    let times: Vec<f64> = offsets.windows(2).map(|w| w[1].0 - w[0].0).collect();
    let values: Vec<f64> = offsets.windows(2).map(|w| w[1].1 - w[0].1).collect();
    let time_glitch = glitches(&times, RESET_TIME);
    let value_glitch = glitches(&values, RESET_OFFSET);
    let mut ranges = Vec::new();
    let mut start = 0;
    for (k, &step) in times.iter().enumerate() {
        if step < 0.0 || (time_glitch[k] && value_glitch[k]) {
            ranges.push((start, k));
            start = k + 1;
        }
    }
    ranges.push((start, offsets.len() - 1));
    ranges
}

/// Which steps lie more than `stds` median absolute deviations and `secs` seconds from the
/// median step.
fn glitches(steps: &[f64], (stds, secs): (f64, f64)) -> Vec<bool> {
    let m = median(steps);
    let deviations: Vec<f64> = steps.iter().map(|s| (s - m).abs()).collect();
    let mad = median(&deviations) + f64::EPSILON;
    steps
        .iter()
        .map(|s| ((s - m) / mad).abs() > stds && (s - m).abs() > secs)
        .collect()
}

/// The median, as numpy's (the mean of the middle two for an even count).
fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n == 0 {
        f64::NAN
    } else if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// The line `offset = intercept + slope · time` through (clock time, offset) pairs, fitted
/// robustly as pyxdf does: Huber loss on values divided by [`WINSOR`], solved with ADMM
/// (Boyd's codes, ρ = 1). `None` if the times don't span a line (all equal).
fn robust_line(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    let n = points.len();
    let a: Vec<f64> = points.iter().map(|p| p.0 / WINSOR).collect();
    let y: Vec<f64> = points.iter().map(|p| p.1 / WINSOR).collect();
    let offset = a.iter().copied().fold(f64::INFINITY, f64::min);
    let a: Vec<f64> = a.iter().map(|v| v - offset).collect();
    // (AᵀA)⁻¹Aᵀ for the design matrix [1, a], through its Cholesky factor as numpy's.
    let (m00, m01, m11) = (
        n as f64,
        a.iter().sum::<f64>(),
        a.iter().map(|v| v * v).sum::<f64>(),
    );
    let l00 = m00.sqrt();
    let l10 = m01 / l00;
    let l11_squared = m11 - l10 * l10;
    if !(l11_squared > 0.0 && l11_squared.is_finite()) {
        return None;
    }
    let det = m00 * m11 - m01 * m01;
    let p0: Vec<f64> = a.iter().map(|&v| (m11 - m01 * v) / det).collect();
    let p1: Vec<f64> = a.iter().map(|&v| (m00 * v - m01) / det).collect();
    let rho = 1.0;
    let thresh = 1.0 + 1.0 / rho;
    let mut u = vec![0.0; n];
    let mut z_minus_u = vec![0.0; n];
    let mut x = (0.0, 0.0);
    for _ in 0..FIT_ITERATIONS {
        let (mut x0, mut x1) = (0.0, 0.0);
        for j in 0..n {
            let target = y[j] + z_minus_u[j];
            x0 += p0[j] * target;
            x1 += p1[j] * target;
        }
        x = (x0, x1);
        for j in 0..n {
            let d = x0 + a[j] * x1 - y[j] + u[j];
            u[j] = d.clamp(-thresh, thresh) / (1.0 + rho);
            z_minus_u[j] = d - 2.0 * u[j];
        }
    }
    let intercept = (x.0 - x.1 * offset) * WINSOR;
    Some((intercept, x.1))
}

/// A segment of a regular stream between breaks, dejittered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Segment {
    /// Its first sample in the stream, and how many it has.
    pub first: u64,
    pub len: u64,
    /// The time of its first sample on its line.
    pub start: f64,
    /// Seconds per sample on its line; `None` for a single sample.
    pub step: Option<f64>,
}

/// Cuts a regular stream's synchronized time stamps, fed in order, into segments at breaks
/// and fits each with its least-squares line.
pub(crate) struct Dejitter {
    tdiff: f64,
    threshold: f64,
    segments: Vec<Segment>,
    current: Option<Fit>,
}

/// The sums of a segment's least-squares fit, of each time stamp's deviation from the
/// nominal line through the segment's first (`r_k = t_k − t_0 − k · tdiff`), which keeps
/// them small.
struct Fit {
    first: u64,
    n: u64,
    t0: f64,
    previous: f64,
    sum: f64,
    weighted: f64,
}

impl Dejitter {
    pub fn new(rate: f64) -> Self {
        let tdiff = 1.0 / rate;
        Dejitter {
            tdiff,
            threshold: BREAK.0.max(BREAK.1 * tdiff),
            segments: Vec::new(),
            current: None,
        }
    }

    /// Feeds the time stamp of the stream's next sample, `sample`.
    pub fn push(&mut self, sample: u64, t: f64) {
        if let Some(fit) = &self.current
            && (t - fit.previous).abs() > self.threshold
        {
            self.finish();
        }
        let fit = self.current.get_or_insert(Fit {
            first: sample,
            n: 0,
            t0: t,
            previous: t,
            sum: 0.0,
            weighted: 0.0,
        });
        let k = fit.n as f64;
        let r = (t - fit.t0) - k * self.tdiff;
        fit.sum += r;
        fit.weighted += k * r;
        fit.previous = t;
        fit.n += 1;
    }

    fn finish(&mut self) {
        let Some(fit) = self.current.take() else {
            return;
        };
        let n = fit.n as f64;
        let segment = if fit.n == 1 {
            Segment {
                first: fit.first,
                len: 1,
                start: fit.t0,
                step: None,
            }
        } else {
            // Least squares on centered indices: slope = Σ idx·r / Σ idx², with Σ idx² =
            // n (n² − 1) / 12, and the line through the mean.
            let half = (n - 1.0) / 2.0;
            let slope = self.tdiff + (fit.weighted - half * fit.sum) / (n * (n * n - 1.0) / 12.0);
            let mean = fit.t0 + half * self.tdiff + fit.sum / n;
            Segment {
                first: fit.first,
                len: fit.n,
                start: mean - slope * half,
                step: Some(slope),
            }
        };
        self.segments.push(segment);
    }

    /// The segments, in order.
    pub fn segments(mut self) -> Vec<Segment> {
        self.finish();
        self.segments
    }
}

/// The effective rate of dejittered segments (pyxdf's): samples over seconds, both summed
/// over segments of more than one sample; `None` if there are none.
pub(crate) fn effective_rate(segments: &[Segment]) -> Option<f64> {
    let (mut samples, mut seconds) = (0.0, 0.0);
    for s in segments {
        if let Some(step) = s.step {
            samples += (s.len - 1) as f64;
            seconds += step * (s.len - 1) as f64;
        }
    }
    (samples > 0.0 && seconds > 0.0).then(|| samples / seconds)
}

/// A stream's grid: its pieces (times relative to `t0`), and where each segment's samples
/// lie in it.
pub(crate) struct Layout {
    pub pieces: Vec<Piece>,
    /// Per segment: its first slot in the grid, its first sample in the stream, its length.
    pub map: Vec<(u64, u64, u64)>,
    /// Slots in the grid, gaps included.
    pub len: u64,
    /// Segments that start before the previous one ends, moved to follow it.
    pub moved: usize,
    /// Segments after a gap longer than [`MAX_GAP_SECONDS`], or with a start that isn't a
    /// number, moved to follow the previous one.
    pub damaged: usize,
}

/// Lays a stream's segments out in a grid: each at its own rate (its line's, or `rate`
/// for a single sample or a line that doesn't rise), the gap before it as many whole slots
/// of the previous piece's rate as fit before its start. A segment starting before the
/// previous one's next sample follows that sample directly instead.
pub(crate) fn layout(segments: &[Segment], rate: f64, t0: f64) -> Layout {
    let mut pieces: Vec<Piece> = Vec::with_capacity(segments.len());
    let mut map = Vec::with_capacity(segments.len());
    let mut moved = 0;
    let mut damaged = 0;
    let mut slot = 0u64;
    for s in segments {
        let piece_rate = match s.step {
            Some(step) if step > 0.0 && (1.0 / step).is_finite() => 1.0 / step,
            _ => rate,
        };
        let mut start = s.start - t0;
        match pieces.last().zip(map.last().map(|m: &(u64, u64, u64)| m.2)) {
            Some((prev, prev_len)) => {
                let next = prev.start + prev_len as f64 / prev.rate;
                let last = prev.start + (prev_len - 1) as f64 / prev.rate;
                if !start.is_finite() || start - next > MAX_GAP_SECONDS {
                    start = next;
                    damaged += 1;
                } else if start > last && start >= next {
                    let gap = ((start - next) * prev.rate).floor();
                    slot = slot.saturating_add(gap.max(0.0) as u64);
                } else if start <= last {
                    start = next;
                    moved += 1;
                }
            }
            None if !start.is_finite() => {
                start = 0.0;
                damaged += 1;
            }
            None => {}
        }
        pieces.push(Piece {
            first: slot,
            start,
            rate: piece_rate,
        });
        map.push((slot, s.first, s.len));
        slot = slot.saturating_add(s.len);
    }
    Layout {
        pieces,
        map,
        len: slot,
        moved,
        damaged,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn robust_line_fits_a_line_and_resists_outliers() {
        let points: Vec<(f64, f64)> = (0..50)
            .map(|k| {
                let t = 1000.0 + 5.0 * k as f64;
                (t, -0.25 + 2e-6 * t)
            })
            .collect();
        let (a, b) = robust_line(&points).unwrap();
        assert!(
            (a + 0.25).abs() < 1e-9 && (b - 2e-6).abs() < 1e-12,
            "{a} {b}"
        );
        let mut noisy = points.clone();
        noisy[10].1 += 0.5;
        noisy[30].1 -= 0.2;
        // pyxdf's own `_robust_fit` gives these: 1000 iterations don't quite absorb
        // outliers of thousands of winsorized units.
        let (a2, b2) = robust_line(&noisy).unwrap();
        assert!((a2 + 0.249_955_815_025_348_8).abs() < 1e-12, "{a2}");
        assert!((b2 - 1.960_669_841_757_041_5e-6).abs() < 1e-17, "{b2}");
        assert_eq!(robust_line(&[(1.0, 0.0), (1.0, 1.0)]), None);
    }

    #[test]
    fn resets_split_where_clock_times_run_back_or_both_jump() {
        let mut offsets: Vec<(f64, f64)> = (0..20).map(|k| (5.0 * k as f64, 0.1)).collect();
        offsets.extend((0..20).map(|k| (3.0 + 5.0 * k as f64, 300.0)));
        assert_eq!(resets(&offsets), [(0, 19), (20, 39)]);
        // A pause in clock times alone, or a jump in offsets alone, isn't a reset.
        let mut late: Vec<(f64, f64)> = (0..20).map(|k| (5.0 * k as f64, 0.1)).collect();
        for p in &mut late[10..] {
            p.0 += 30.0;
        }
        for p in &mut late[15..] {
            p.1 += 30.0;
        }
        assert_eq!(resets(&late), [(0, 19)]);
        // Both at once is.
        let mut both: Vec<(f64, f64)> = (0..20).map(|k| (5.0 * k as f64, 0.1)).collect();
        for p in &mut both[10..] {
            p.0 += 100.0;
            p.1 += 50.0;
        }
        assert_eq!(resets(&both), [(0, 9), (10, 19)]);
        // Clock times running backwards are a reset, even without a jump in offsets.
        let mut back: Vec<(f64, f64)> = (0..20).map(|k| (5.0 * k as f64, 0.1)).collect();
        for p in &mut back[10..] {
            p.0 -= 6.0;
        }
        assert_eq!(resets(&back), [(0, 9), (10, 19)]);
    }

    #[test]
    fn dejittered_segments_are_least_squares_lines() {
        let mut d = Dejitter::new(100.0);
        let jitter = [0.002, -0.001, 0.0, 0.003, -0.004];
        for k in 0..1000u64 {
            d.push(k, 10.0 + k as f64 * 0.010_01 + jitter[k as usize % 5]);
        }
        // At 100 Hz a step of 0.99 s is no break, one of 1.01 s is; at 0.25 Hz a step of
        // 7.9 s is none (less than 2 periods), one of 8.1 s is. Then a break of 10 s, a single
        // sample, and a backward jump.
        let breaks = |rate: f64, step: f64| {
            let mut d = Dejitter::new(rate);
            for k in 0..20u64 {
                let late = if k >= 10 { step - 1.0 / rate } else { 0.0 };
                d.push(k, k as f64 / rate + late);
            }
            d.segments().len() - 1
        };
        assert_eq!(breaks(100.0, 0.99), 0);
        assert_eq!(breaks(100.0, 1.01), 1);
        assert_eq!(breaks(0.25, 7.9), 0);
        assert_eq!(breaks(0.25, 8.1), 1);
        d.push(1000, 30.0);
        d.push(1001, 20.0);
        d.push(1002, 20.01);
        let segments = d.segments();
        assert_eq!(segments.len(), 3);
        let s = segments[0];
        assert_eq!((s.first, s.len), (0, 1000));
        // Brute force.
        let ts: Vec<f64> = (0..1000)
            .map(|k| 10.0 + k as f64 * 0.010_01 + jitter[k % 5])
            .collect();
        let mean = ts.iter().sum::<f64>() / 1000.0;
        let idx = |k: usize| k as f64 - 499.5;
        let slope = (0..1000).map(|k| idx(k) * (ts[k] - mean)).sum::<f64>()
            / (0..1000).map(|k| idx(k) * idx(k)).sum::<f64>();
        assert!((s.step.unwrap() - slope).abs() < 1e-15);
        assert!((s.start - (mean - slope * 499.5)).abs() < 1e-12);
        assert_eq!(
            segments[1],
            Segment {
                first: 1000,
                len: 1,
                start: 30.0,
                step: None
            }
        );
        assert_eq!((segments[2].first, segments[2].len), (1001, 2));
        let rate = effective_rate(&segments).unwrap();
        assert!(
            (rate - 1000.0 / (slope * 999.0 + 0.01)).abs() < 1e-6,
            "{rate}"
        );
    }

    #[test]
    fn layout_places_gaps_in_whole_slots_and_moves_overlaps() {
        let seg = |first, len, start, step| Segment {
            first,
            len,
            start,
            step,
        };
        let segments = [
            seg(0, 100, 10.0, Some(0.01)),
            // 2.5 s after the next sample at 11.0: 250 slots.
            seg(100, 50, 13.505, Some(0.02)),
            // Before the previous one ends: follows it.
            seg(150, 10, 13.6, Some(0.02)),
            seg(160, 1, 100.0, None),
        ];
        let l = layout(&segments, 50.0, 10.0);
        // 85.295 s after the next sample at 4.705: 4264 whole slots of 50 Hz.
        assert_eq!(
            l.map,
            [(0, 0, 100), (350, 100, 50), (400, 150, 10), (4674, 160, 1)]
        );
        assert!((l.pieces[1].start - 3.505).abs() < 1e-12);
        assert!((l.pieces[2].start - (3.505 + 50.0 * 0.02)).abs() < 1e-12);
        assert_eq!(l.pieces[3].rate, 50.0);
        assert_eq!(l.moved, 1);
        assert_eq!(l.len, 4675);
    }
}
