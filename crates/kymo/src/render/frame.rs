//! The CPU side of trace drawing: where each tile goes on screen and how its native values
//! become pixel rows. Everything the shader needs is computed here in f64 and handed over
//! as small f32 numbers, so huge offsets and long recordings keep sub-pixel precision.

use std::collections::HashMap;
use std::ops::{Range, RangeInclusive};
use std::sync::Arc;

use kymo_core::tile::{ChannelBlock, Level, Placed, Query, Tile, TileData};
use kymo_core::{ChannelId, ChannelKind, Grid, Recording, SampleType};

use crate::view::ViewState;

/// Pixels by which a tile's columns reach past its data on either side, so that the
/// shader's f32 positions never put an item in a column the f64 placement left out.
pub const EDGE_TOLERANCE: f32 = 1e-3;

/// Pixels a clipped row's bottom edge is moved up, so that a trace along it stays in the
/// row's last pixel row instead of reaching into the next row's first.
const CLIP_INSET: f64 = 1e-3;

/// One channel's row in the trace area. Positions are physical pixels from the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub channel: ChannelId,
    /// Where `baseline` is drawn: the middle of the row.
    pub center: f64,
    /// Height of the row; the loading hatch fills it.
    pub height: f64,
    /// Pixels per physical unit; larger values are drawn higher.
    pub px_per_unit: f64,
    /// The physical value drawn at `center`: the channel's DC value with DC removal on,
    /// otherwise 0.
    pub baseline: f64,
    /// Premultiplied RGBA, written as given.
    pub color: [f32; 4],
    /// Whether the trace stays inside the row, drawn along its edge where it would leave
    /// it (stim channels, whose codes reach far beyond one row at their scale).
    pub clip: bool,
}

/// What the trace area shows, in its own pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    /// Seconds since the recording start, left edge to right edge.
    pub time: Range<f64>,
    /// Width and height of the trace area in physical pixels.
    pub size: [u32; 2],
    /// The channels drawn; others are skipped.
    pub rows: Vec<Row>,
    /// Premultiplied RGBA of the hatch over parts that are still loading.
    pub hatch: [f32; 4],
}

impl Scene {
    /// The visible rows of `view` in a trace area of `size` physical pixels, all in
    /// `color`, without DC removal. As in MNE, a channel's amplitude fills half a row:
    /// ±amplitude spans the row.
    pub fn from_view(view: &ViewState, size: [u32; 2], color: [f32; 4]) -> Scene {
        let height = f64::from(size[1]);
        let row_height = view.row_height(height);
        let rows = view
            .visible_rows()
            .zip(view.visible_channels())
            .map(|(i, &channel)| {
                let amplitude = view.channel_amplitude(channel);
                Row {
                    channel,
                    center: view.y_of_row(i as f64 + 0.5, height),
                    height: row_height,
                    px_per_unit: if amplitude > 0.0 && amplitude.is_finite() {
                        row_height / (2.0 * amplitude)
                    } else {
                        0.0
                    },
                    baseline: 0.0,
                    color,
                    clip: view.kind_of(channel) == ChannelKind::Stim,
                }
            })
            .collect();
        Scene {
            time: view.time(),
            size,
            rows,
            hatch: color.map(|c| c * 0.25),
        }
    }
}

/// A native sample value: integers (int16, int32) widened to i32, or f32.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Native {
    Int(i32),
    Float(f32),
}

impl Native {
    /// The value's bits as the shader reads them.
    pub fn bits(self) -> u32 {
        match self {
            Native::Int(v) => v as u32,
            Native::Float(v) => v.to_bits(),
        }
    }
}

/// How one channel's native values become y positions:
/// `y = offset + (value − reference) · scale`, with the difference taken in the native
/// type (exactly for integers, saturating) before it becomes an f32.
///
/// The reference is the native value nearest the row's baseline, so the f32 part only
/// covers what is on screen: a 1e6 µV offset costs no precision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefficients {
    pub reference: Native,
    /// Pixels per native unit; negative, because larger values go up.
    pub scale: f32,
    /// y of `reference`.
    pub offset: f32,
}

impl Coefficients {
    /// For a channel of `sample_type` with physical value = native · `gain` + `offset`,
    /// drawn in `row`.
    pub fn new(sample_type: SampleType, gain: f64, offset: f64, row: &Row) -> Coefficients {
        let usable = gain.is_finite() && gain != 0.0;
        // The native value closest to the baseline (0 without a usable gain).
        let target = if usable {
            (row.baseline - offset) / gain
        } else {
            0.0
        };
        let reference = match sample_type {
            SampleType::F32 => Native::Float(if target.is_finite() {
                target as f32
            } else {
                0.0
            }),
            // Float-to-int casts saturate, and NaN becomes 0.
            _ => Native::Int(target.round() as i32),
        };
        let at_reference = match reference {
            Native::Int(r) => f64::from(r),
            Native::Float(r) => f64::from(r),
        };
        let gain = if usable { gain } else { 0.0 };
        // Physical value at the reference, relative to the baseline: within half a native
        // step for integers.
        let rest = at_reference * gain + offset - row.baseline;
        Coefficients {
            reference,
            scale: (-gain * row.px_per_unit) as f32,
            offset: (row.center - rest * row.px_per_unit) as f32,
        }
    }

    /// The y position of `value` as the shader computes it, in f32; `None` for values the
    /// shader skips (NaN and infinities) or of the wrong type.
    pub fn y(&self, value: Native) -> Option<f32> {
        let delta = match (value, self.reference) {
            (Native::Int(v), Native::Int(r)) => v.saturating_sub(r) as f32,
            (Native::Float(v), Native::Float(r)) if v.is_finite() => v - r,
            _ => return None,
        };
        Some(self.offset + delta * self.scale)
    }
}

/// One channel of a tile to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelDraw {
    /// The channel's row in the tile.
    pub row: u32,
    pub coefficients: Coefficients,
    /// Premultiplied RGBA.
    pub color: [f32; 4],
    /// The y range (top, bottom) the trace is clamped to, if its row clips it.
    pub clip: Option<[f32; 2]>,
    /// The first item of the next tile, when [`TileDraw::continues`]: its sample twice at
    /// level 0, or its bucket's minimum and maximum. `None` in a gap.
    pub next: Option<[Native; 2]>,
}

/// A tile placed on screen.
///
/// Items are samples at level 0 and buckets above. Item `i` starts at
/// `anchor_x + (i − anchor) · item_width` pixels, a bucket's envelope reaching to the start
/// of the next one.
#[derive(Clone, Debug)]
pub struct TileDraw {
    pub tile: Arc<Tile>,
    /// Pixel columns that may show the tile.
    pub columns: Range<u32>,
    /// The items to draw. If the tile [`continues`](Self::continues), the last of them
    /// stands for the next tile's first item ([`ChannelDraw::next`]).
    pub items: RangeInclusive<u32>,
    /// Whether the next tile follows directly, so the line or envelope runs on into it.
    pub continues: bool,
    /// An item near the left edge of the screen, for precise positions.
    pub anchor: u32,
    pub anchor_x: f32,
    /// Pixels per item.
    pub item_width: f32,
    /// 1 / `item_width`, computed in f64.
    pub inverse_width: f32,
    /// The x range the drawn samples cover: from the first to the last, or to the next
    /// tile's first if the tile [`continues`](Self::continues).
    /// [`columns`](Self::columns) are the pixels it touches.
    pub domain: [f32; 2],
    pub channels: Vec<ChannelDraw>,
}

/// A hatched rectangle over samples that have no tile yet: x0, y0, x1, y1 in pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Hatch {
    pub rect: [f32; 4],
    pub color: [f32; 4],
}

/// Everything the renderer draws in one frame.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    /// The trace area in physical pixels.
    pub size: [u32; 2],
    pub draws: Vec<TileDraw>,
    pub hatches: Vec<Hatch>,
}

/// Seconds to pixels.
struct Pixels {
    start: f64,
    per_second: f64,
}

impl Pixels {
    fn x(&self, grid: &Grid, sample: u64) -> f64 {
        (grid.time_of(sample) - self.start) * self.per_second
    }
}

impl Frame {
    /// Places the tiles of `query` (for the view `scene` shows) and the loading hatches.
    /// Channels without a row in `scene` are skipped.
    pub fn new(recording: &Recording, scene: &Scene, query: &Query) -> Frame {
        let mut frame = Frame {
            size: scene.size,
            ..Frame::default()
        };
        let [width, height] = scene.size;
        let span = scene.time.end - scene.time.start;
        if width == 0 || height == 0 || !(span > 0.0 && span.is_finite()) {
            return frame;
        }
        let pixels = Pixels {
            start: scene.time.start,
            per_second: f64::from(width) / span,
        };
        let rows: HashMap<ChannelId, &Row> = scene.rows.iter().map(|r| (r.channel, r)).collect();

        for (i, placed) in query.draw.iter().enumerate() {
            let next = query.draw.get(i + 1);
            if let Some(draw) = tile_draw(recording, &pixels, &rows, width, placed, next) {
                frame.draws.push(draw);
            }
        }

        let (width, height) = (f64::from(width), f64::from(height));
        for loading in &query.loading {
            let Some(grid) = block_grid(recording, loading.block) else {
                continue;
            };
            let x0 = pixels.x(grid, loading.samples.start).max(0.0);
            let x1 = pixels.x(grid, loading.samples.end).min(width);
            if (x0..x1).is_empty() {
                continue;
            }
            for id in loading.block.channels(grid) {
                let Some(row) = rows.get(id) else { continue };
                let y0 = (row.center - row.height / 2.0).max(0.0);
                let y1 = (row.center + row.height / 2.0).min(height);
                if y0 < y1 {
                    frame.hatches.push(Hatch {
                        rect: [x0 as f32, y0 as f32, x1 as f32, y1 as f32],
                        color: scene.hatch,
                    });
                }
            }
        }
        frame
    }
}

fn block_grid(recording: &Recording, block: ChannelBlock) -> Option<&Grid> {
    recording
        .grids()
        .get(block.grid.0 as usize)
        .filter(|grid| block.index < ChannelBlock::count(grid))
}

fn tile_draw(
    recording: &Recording,
    pixels: &Pixels,
    rows: &HashMap<ChannelId, &Row>,
    width: u32,
    placed: &Placed,
    next: Option<&Placed>,
) -> Option<TileDraw> {
    let tile = &placed.tile;
    let key = tile.key();
    let grid = block_grid(recording, key.block)?;
    let bucket = key.level.bucket_len();
    let first = key.sample_range().start;
    let tile_end = first + tile.len() as u64 * bucket;
    let samples = placed.samples.start.max(first)..placed.samples.end.min(tile_end);
    if samples.is_empty() || samples.end > grid.len() {
        return None;
    }
    let x = |sample: u64| pixels.x(grid, sample);
    let raw = key.level == Level::RAW;

    let lo = (samples.start - first) / bucket;
    let last = (samples.end - 1 - first) / bucket;
    // The drawing runs on into the next tile when that one follows directly, and its first
    // item fits: a level-0 line only continues to a sample, not to an envelope.
    let next = next.filter(|n| {
        let k = n.tile.key();
        k.block == key.block
            && n.samples.start == samples.end
            && (samples.end - first).is_multiple_of(bucket)
            && (!raw || k.level == Level::RAW)
            && !n.tile.is_empty()
    });
    let (hi, end) = match next {
        Some(_) => (last + 1, x(samples.end)),
        None => (last, x(samples.end - 1)),
    };
    let domain = [x(samples.start), end];
    let tolerance = f64::from(EDGE_TOLERANCE);
    let columns = (domain[0] - tolerance).floor().max(0.0)
        ..((domain[1] + tolerance).floor() + 1.0).min(f64::from(width));
    if columns.is_empty() {
        return None;
    }

    let item_width = bucket as f64 / grid.rate() * pixels.per_second;
    let anchor = (-x(first) / item_width).floor().clamp(lo as f64, hi as f64) as u64;

    let channels: Vec<ChannelDraw> = key
        .block
        .channels(grid)
        .iter()
        .enumerate()
        .filter_map(|(r, id)| {
            let row = rows.get(id)?;
            let (gain, offset) = recording.channel(*id).tile_scaling();
            Some(ChannelDraw {
                row: r as u32,
                coefficients: Coefficients::new(grid.sample_type(), gain, offset, row),
                clip: row.clip.then(|| {
                    let half = row.height / 2.0;
                    [
                        (row.center - half) as f32,
                        (row.center + half - CLIP_INSET) as f32,
                    ]
                }),
                color: row.color,
                next: next.and_then(|n| first_item(n, r)),
            })
        })
        .collect();
    if channels.is_empty() {
        return None;
    }

    Some(TileDraw {
        tile: Arc::clone(tile),
        columns: columns.start as u32..columns.end as u32,
        items: lo as u32..=hi as u32,
        continues: next.is_some(),
        anchor: anchor as u32,
        anchor_x: x(first + anchor * bucket) as f32,
        item_width: item_width as f32,
        inverse_width: (1.0 / item_width) as f32,
        domain: domain.map(|d| d as f32),
        channels,
    })
}

/// The first item `placed` draws of channel `row`: a sample twice, or a bucket's minimum and
/// maximum; `None` in a gap.
fn first_item(placed: &Placed, row: usize) -> Option<[Native; 2]> {
    let tile = &placed.tile;
    let key = tile.key();
    let item = (placed.samples.start - key.sample_range().start) / key.level.bucket_len();
    let item = u32::try_from(item).ok()?;
    if !tile.valid().iter().any(|v| v.contains(&item)) {
        return None;
    }
    let at = |i: usize| match tile.data() {
        TileData::I16(d) => d.get(i).map(|&v| Native::Int(i32::from(v))),
        TileData::I32(d) => d.get(i).map(|&v| Native::Int(v)),
        TileData::F32(d) => d.get(i).map(|&v| Native::Float(v)),
    };
    let i = row * tile.len() + item as usize;
    if key.level == Level::RAW {
        at(i).map(|v| [v, v])
    } else {
        Some([at(2 * i)?, at(2 * i + 1)?])
    }
}
