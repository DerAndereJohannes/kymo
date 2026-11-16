// Trace drawing. One instance per (channel, pixel column) of a tile draw: it finds the
// samples or buckets that reach its column and draws a one-pixel-wide quad over the rows
// they reach. Quad corners lie on pixel edges, so a peak can never fall between pixel
// centers and vanish.
//
// A draw reads its tile and the tiles placed right before and after it (`parts`), as one
// run of items (`fetch`), so lines and envelopes run on across tile boundaries. Neighboring
// draws never share a column.
//
// Without anti-aliasing, a column lights every row between the lowest and highest point
// the trace reaches in it. With anti-aliasing, each pixel is covered by how close its
// center lies to the trace drawn one pixel wide: 1 − distance, so a pixel the trace runs
// through the middle of is covered fully and one a pixel away not at all. The trace is the
// line through the samples at level 0; above, each bucket's envelope reaching to the next.
//
// The distance comes from slices: the vertex stage cuts the two pixels around a column's
// center into SLICES slices and records the y range the trace reaches in each, once per
// column; a pixel then takes its distance to those boxes. The trace runs through all of
// each box's height, so this is the distance to the trace, or less by up to a slice's width
// (2/SLICES pixels): a pixel is covered at most 2/SLICES more than exactly, never less,
// whatever the number of samples per pixel.
//
// Positions are physical pixels of the trace area, y down. The CPU side
// (`render/frame.rs`) computes every parameter in f64 relative to an anchor near the
// screen, so the f32 numbers here stay small.

struct Globals {
    size: vec2<f32>,
    // 1 to anti-alias traces, 0 to draw them in whole pixels.
    antialias: f32,
    // The word of `frame` where the item positions start, after the slots.
    positions: u32,
};

// One channel of a tile draw, 12 words in `frame`.
struct Slot {
    color: vec4<f32>,
    // The channel's row in the tiles.
    row: u32,
    // Native value bits drawn at `offset`.
    reference: u32,
    // Pixels per native unit.
    scale: f32,
    offset: f32,
    // The y range (top, bottom) the trace is clamped to: the row for stim channels,
    // everything for others.
    clip_top: f32,
    clip_bottom: f32,
    _pad0: u32,
    _pad1: u32,
};

// Where one tile's items lie: item `i` starts at `anchor_x + (i - anchor) * item_width`,
// or for an irregular time base at `positions[xs + i - first]`, ending at the next one.
struct Part {
    anchor_x: f32,
    item_width: f32,
    inverse_width: f32,
    anchor: i32,
    // The items that may be read.
    first: i32,
    last: i32,
    // Items per channel in the tile.
    len: u32,
    // 1 at levels >= 1: a (min, max) pair per bucket.
    pairs: u32,
    // First data word, after the validity bits.
    data: u32,
    // 0 if there is no such tile.
    present: u32,
    // The first of an irregular part's positions.
    xs: u32,
    // IRREGULAR and HOLD.
    flags: u32,
};

// The part's items lie at the positions in `frame`.
const IRREGULAR: u32 = 1u;
// Its samples hold their values until the next one's start: each is drawn as an envelope
// of its one value, reaching to the next.
const HOLD: u32 = 2u;

// One tile draw.
struct Draw {
    // The tile before, the tile, the tile after.
    parts: array<Part, 3>,
    column0: u32,
    columns: u32,
    slot0: u32,
    // 0: int16 (two per word), 1: int32, 2: float32.
    kind: u32,
};

struct Hatch {
    rect: vec4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
// The frame's slots, then the item positions of irregular parts (f32 bits).
@group(0) @binding(1) var<storage, read> frame: array<u32>;
@group(0) @binding(2) var<uniform> draw: Draw;
// Per tile: validity bits (one per item, shared by all channels), then the values.
@group(1) @binding(0) var<storage, read> tile0: array<u32>;
@group(1) @binding(1) var<storage, read> tile1: array<u32>;
@group(1) @binding(2) var<storage, read> tile2: array<u32>;
// The hatch pipeline's own group.
@group(1) @binding(3) var<storage, read> hatches: array<Hatch>;

// Slot `i` of the frame.
fn slot_at(i: u32) -> Slot {
    let w = 12u * i;
    var s: Slot;
    s.color = vec4<f32>(bitcast<f32>(frame[w]), bitcast<f32>(frame[w + 1u]),
        bitcast<f32>(frame[w + 2u]), bitcast<f32>(frame[w + 3u]));
    s.row = frame[w + 4u];
    s.reference = frame[w + 5u];
    s.scale = bitcast<f32>(frame[w + 6u]);
    s.offset = bitcast<f32>(frame[w + 7u]);
    s.clip_top = bitcast<f32>(frame[w + 8u]);
    s.clip_bottom = bitcast<f32>(frame[w + 9u]);
    return s;
}

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    // The instance, for the fragment stage to find its slot and column (traces only).
    @location(1) @interpolate(flat) instance: u32,
    // y in the trace area, which the fragment's position isn't: the area is a viewport
    // inside the window (traces only).
    @location(2) y: f32,
    // With anti-aliasing, the y range (top, bottom) of each slice, two per vector; empty
    // (top > bottom) where the trace doesn't reach.
    @location(3) @interpolate(flat) s0: vec4<f32>,
    @location(4) @interpolate(flat) s1: vec4<f32>,
    @location(5) @interpolate(flat) s2: vec4<f32>,
    @location(6) @interpolate(flat) s3: vec4<f32>,
    @location(7) @interpolate(flat) s4: vec4<f32>,
    @location(8) @interpolate(flat) s5: vec4<f32>,
    @location(9) @interpolate(flat) s6: vec4<f32>,
    @location(10) @interpolate(flat) s7: vec4<f32>,
};

// Slices across the two pixels around a column's center.
const SLICES: i32 = 16;

const KIND_I16: u32 = 0u;
const KIND_F32: u32 = 2u;

fn clip(p: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(p.x / globals.size.x * 2.0 - 1.0, 1.0 - p.y / globals.size.y * 2.0, 0.0, 1.0);
}

// A degenerate quad outside the screen.
fn nothing() -> Out {
    var out: Out;
    out.position = vec4<f32>(-2.0, -2.0, 0.0, 1.0);
    out.color = vec4<f32>(0.0);
    out.instance = 0u;
    out.y = 0.0;
    out.s0 = vec4<f32>(0.0);
    out.s1 = vec4<f32>(0.0);
    out.s2 = vec4<f32>(0.0);
    out.s3 = vec4<f32>(0.0);
    out.s4 = vec4<f32>(0.0);
    out.s5 = vec4<f32>(0.0);
    out.s6 = vec4<f32>(0.0);
    out.s7 = vec4<f32>(0.0);
    return out;
}

// Rounded to an integer, kept far from i32 overflow.
fn to_int(x: f32) -> i32 {
    return i32(clamp(x, -1.0e9, 1.0e9));
}

fn word(part: u32, i: u32) -> u32 {
    if part == 0u {
        return tile0[i];
    }
    if part == 1u {
        return tile1[i];
    }
    return tile2[i];
}

// The bits of value `index` of a tile, integers sign-extended to 32 bits.
fn bits_at(part: u32, index: u32) -> u32 {
    let data = draw.parts[part].data;
    if draw.kind == KIND_I16 {
        let w = word(part, data + (index >> 1u));
        let shift = (index & 1u) * 16u;
        // Move the value into the top half, then shift it back down with its sign.
        return bitcast<u32>(bitcast<i32>(w << (16u - shift)) >> 16u);
    }
    return word(part, data + index);
}

// y of a value and 1, or 0 in the second component for values to skip (NaN, infinity).
fn y_of(bits: u32, slot: Slot) -> vec2<f32> {
    var delta: f32;
    if draw.kind == KIND_F32 {
        if (bits & 0x7f800000u) == 0x7f800000u {
            return vec2<f32>(0.0, 0.0);
        }
        delta = bitcast<f32>(bits) - bitcast<f32>(slot.reference);
    } else {
        let v = bitcast<i32>(bits);
        let r = bitcast<i32>(slot.reference);
        // Wrapping subtraction (well defined in u32), then saturation on overflow: the
        // operands' signs differ and the result's sign isn't v's.
        var d = bitcast<i32>(bits - slot.reference);
        if ((v ^ r) & (v ^ d)) < 0 {
            d = select(i32(-2147483647) - 1, 2147483647, v >= 0);
        }
        delta = f32(d);
    }
    return vec2<f32>(slot.offset + delta * slot.scale, 1.0);
}

// Items of part `p` that may be read.
fn count(p: u32) -> i32 {
    let part = draw.parts[p];
    return select(0, part.last - part.first + 1, part.present != 0u);
}

// The run index of item `local` of part `p`: the tile's first readable item is 0, the
// tile before's come before it, the tile after's after the tile's last.
fn run_index(p: u32, local: i32) -> i32 {
    let part = draw.parts[p];
    if p == 0u {
        return local - part.first - count(0u);
    }
    if p == 1u {
        return local - part.first;
    }
    return count(1u) + local - part.first;
}

// One sample or bucket: where it starts and how wide it is, and its y range.
struct Item {
    x: f32,
    width: f32,
    // y of the highest and lowest value (top < bottom); a sample has both the same.
    top: f32,
    bottom: f32,
    ok: bool,
    pairs: bool,
};

// Item `k` of the run, for channel `slot`; not ok in gaps, for skipped values and past the
// readable items.
fn fetch(slot: Slot, k: i32) -> Item {
    var p = 1u;
    var local: i32;
    let n0 = count(0u);
    let n1 = count(1u);
    if k < 0 {
        p = 0u;
        local = draw.parts[0].first + n0 + k;
    } else if k < n1 {
        local = draw.parts[1].first + k;
    } else {
        p = 2u;
        local = draw.parts[2].first + k - n1;
    }
    let part = draw.parts[p];
    var item: Item;
    item.ok = false;
    item.pairs = part.pairs != 0u || (part.flags & HOLD) != 0u;
    if (part.flags & IRREGULAR) != 0u {
        // Positions exist for the readable items only (and the end of the last).
        item.x = 0.0;
        item.width = 0.0;
        if part.present == 0u || local < part.first || local > part.last {
            return item;
        }
        item.x = position(part, local);
        item.width = position(part, local + 1) - item.x;
    } else {
        item.x = part.anchor_x + f32(local - part.anchor) * part.item_width;
        item.width = part.item_width;
        if part.present == 0u || local < part.first || local > part.last {
            return item;
        }
    }
    let i = u32(local);
    if ((word(p, i >> 5u) >> (i & 31u)) & 1u) == 0u {
        return item;
    }
    if part.pairs != 0u {
        let index = 2u * (slot.row * part.len + i);
        let a = y_of(bits_at(p, index), slot);
        let b = y_of(bits_at(p, index + 1u), slot);
        item.ok = a.y > 0.0 && b.y > 0.0;
        item.top = min(a.x, b.x);
        item.bottom = max(a.x, b.x);
    } else {
        let a = y_of(bits_at(p, slot.row * part.len + i), slot);
        item.ok = a.y > 0.0;
        item.top = a.x;
        item.bottom = a.x;
    }
    return item;
}

// Where item `local` of an irregular part starts (the last readable item's end at `last + 1`).
fn position(part: Part, local: i32) -> f32 {
    return bitcast<f32>(frame[globals.positions + part.xs + u32(local - part.first)]);
}

// The last readable item of an irregular part starting at or before x: `first - 1` if
// none, `last + 1` if x lies at or after the last one's end (as a regular part's index of x
// lies past its items).
fn last_at(part: Part, x: f32) -> i32 {
    var lo = part.first;
    var hi = part.last + 2;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if position(part, mid) <= x {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    return lo - 1;
}

// The run indices of the items that can reach x in xa..=xb, with one more on either side
// against rounding at item boundaries.
fn items_near(xa: f32, xb: f32) -> vec2<i32> {
    var k0 = 1073741824;
    var k1 = -1073741824;
    for (var p = 0u; p < 3u; p++) {
        let part = draw.parts[p];
        if part.present == 0u {
            continue;
        }
        var l0: i32;
        var l1: i32;
        if (part.flags & IRREGULAR) != 0u {
            l0 = max(part.first, last_at(part, xa));
            l1 = min(part.last, last_at(part, xb));
        } else {
            l0 = max(part.first, part.anchor + to_int(floor((xa - part.anchor_x) * part.inverse_width)));
            l1 = min(part.last, part.anchor + to_int(floor((xb - part.anchor_x) * part.inverse_width)));
        }
        if l0 <= l1 {
            k0 = min(k0, run_index(p, l0));
            k1 = max(k1, run_index(p, l1));
        }
    }
    return vec2<i32>(max(k0 - 1, -count(0u)), min(k1 + 1, count(1u) + count(2u) - 1));
}

// y of the line from `a` to `b` (a.x < b.x) at x in a.x..=b.x, from the nearer end: an
// end on screen keeps its precision however far off screen the other lies (from the far
// end, a lerp like `mix` rounds it to the far end's precision, 8 px at 1e8 px, as WARP
// does).
fn along(a: vec2<f32>, b: vec2<f32>, x: f32) -> f32 {
    if x - a.x <= b.x - x {
        return a.y + (b.y - a.y) * ((x - a.x) / (b.x - a.x));
    }
    return b.y + (a.y - b.y) * ((b.x - x) / (b.x - a.x));
}

// Whether the trace runs on from item `a` to the next item `b`. A line runs between two
// samples only (a level-0 line doesn't run on into a coarser stand-in's envelope); an
// envelope reaches to the start of whatever follows.
fn joined(a: Item, b: Item) -> bool {
    return a.ok && b.ok;
}

// The y range (top, bottom) the trace reaches over x in xa..=xb: samples inside, the
// line between samples where it crosses the range's edges, and buckets reaching into it,
// a bucket reaching to the start of the next. Empty (top > bottom) if nothing does.
fn reach(slot: Slot, xa: f32, xb: f32) -> vec2<f32> {
    var top = 3.0e38;
    var bottom = -3.0e38;
    let ks = items_near(xa, xb);
    var item = fetch(slot, ks.x);
    for (var k = ks.x; k <= ks.y; k++) {
        let next = fetch(slot, k + 1);
        if item.ok {
            if item.pairs {
                if item.x <= xb && item.x + item.width > xa {
                    top = min(top, item.top);
                    bottom = max(bottom, item.bottom);
                }
            } else if item.x >= xa && item.x <= xb {
                top = min(top, item.top);
                bottom = max(bottom, item.top);
            }
            // The line to the next sample, where it crosses the range's edges.
            if !item.pairs && !next.pairs && joined(item, next) && item.x <= xb && next.x >= xa {
                for (var e = 0; e < 2; e++) {
                    let x = select(xa, xb, e == 1);
                    if x > item.x && x < next.x {
                        let y = along(vec2<f32>(item.x, item.top), vec2<f32>(next.x, next.top), x);
                        top = min(top, y);
                        bottom = max(bottom, y);
                    }
                }
            }
        }
        item = next;
    }
    return vec2<f32>(top, bottom);
}

// The slice holding x, for slices from `x0` on; may lie outside 0..SLICES.
fn slice_of(x: f32, x0: f32) -> i32 {
    return to_int(floor((x - x0) * f32(SLICES) * 0.5));
}

// The y range (top, bottom) the trace reaches in each slice of x0..x0 + 2: as `reach`
// computes it for a column, for each slice.
fn slices(slot: Slot, x0: f32) -> array<vec2<f32>, 16> {
    var s: array<vec2<f32>, 16>;
    for (var i = 0; i < SLICES; i++) {
        s[i] = vec2<f32>(3.0e38, -3.0e38);
    }
    let width = 2.0 / f32(SLICES);
    let ks = items_near(x0, x0 + 2.0);
    var item = fetch(slot, ks.x);
    for (var k = ks.x; k <= ks.y; k++) {
        let next = fetch(slot, k + 1);
        if item.ok {
            var i0: i32;
            var i1: i32;
            if item.pairs {
                // The bucket, reaching to the next.
                i0 = slice_of(item.x, x0);
                i1 = slice_of(item.x + item.width, x0);
            } else {
                i0 = slice_of(item.x, x0);
                i1 = i0;
            }
            for (var i = max(i0, 0); i <= min(i1, SLICES - 1); i++) {
                s[i] = vec2<f32>(min(s[i].x, item.top), max(s[i].y, item.bottom));
            }
            // The line to the next sample, in each slice it crosses.
            if !item.pairs && !next.pairs && joined(item, next) {
                let a = vec2<f32>(item.x, item.top);
                let b = vec2<f32>(next.x, next.top);
                let j0 = max(slice_of(a.x, x0), 0);
                let j1 = min(slice_of(b.x, x0), SLICES - 1);
                for (var i = j0; i <= j1; i++) {
                    let xa = clamp(x0 + f32(i) * width, a.x, b.x);
                    let xb = clamp(x0 + f32(i + 1) * width, a.x, b.x);
                    var ya = a.y;
                    var yb = b.y;
                    if b.x > a.x {
                        ya = along(a, b, xa);
                        yb = along(a, b, xb);
                    }
                    s[i] = vec2<f32>(min(s[i].x, min(ya, yb)), max(s[i].y, max(ya, yb)));
                }
            }
        }
        item = next;
    }
    return s;
}

@vertex
fn vs_trace(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> Out {
    let slot = slot_at(draw.slot0 + instance / draw.columns);
    let column = f32(draw.column0 + instance % draw.columns);
    let aa = globals.antialias > 0.5;
    var lo: f32;
    var hi: f32;
    var s: array<vec2<f32>, 16>;
    if aa {
        // Anti-aliased pixels see the trace up to a pixel from their centers.
        s = slices(slot, column - 0.5);
        lo = 3.0e38;
        hi = -3.0e38;
        for (var i = 0; i < SLICES; i++) {
            lo = min(lo, s[i].x);
            hi = max(hi, s[i].y);
        }
    } else {
        let span = reach(slot, column, column + 1.0);
        lo = span.x;
        hi = span.y;
    }
    if lo > hi {
        return nothing();
    }
    // A clipped trace runs along the row's edge where it would leave the row.
    lo = clamp(lo, slot.clip_top, slot.clip_bottom);
    hi = clamp(hi, slot.clip_top, slot.clip_bottom);
    // Whole rows: from the row holding the highest point to the one holding the lowest,
    // and with anti-aliasing a pixel more on either side, within the rows the clip allows.
    var top = floor(clamp(lo, -2.0, globals.size.y + 2.0));
    var bottom = floor(clamp(hi, -2.0, globals.size.y + 2.0)) + 1.0;
    if aa {
        top = max(top - 1.0, floor(max(slot.clip_top, -2.0)));
        bottom = min(bottom + 1.0, floor(min(slot.clip_bottom, globals.size.y + 2.0)) + 1.0);
    }
    let corner = vec2<f32>(f32(vertex & 1u), f32(vertex >> 1u));
    let y = mix(top, bottom, corner.y);
    var out: Out;
    out.position = clip(vec2<f32>(column + corner.x, y));
    out.color = slot.color;
    out.instance = instance;
    out.y = y;
    out.s0 = vec4<f32>(s[0], s[1]);
    out.s1 = vec4<f32>(s[2], s[3]);
    out.s2 = vec4<f32>(s[4], s[5]);
    out.s3 = vec4<f32>(s[6], s[7]);
    out.s4 = vec4<f32>(s[8], s[9]);
    out.s5 = vec4<f32>(s[10], s[11]);
    out.s6 = vec4<f32>(s[12], s[13]);
    out.s7 = vec4<f32>(s[14], s[15]);
    return out;
}

@fragment
fn fs_trace(input: Out) -> @location(0) vec4<f32> {
    if globals.antialias < 0.5 {
        return input.color;
    }
    let slot = slot_at(draw.slot0 + input.instance / draw.columns);
    let column = f32(draw.column0 + input.instance % draw.columns);
    let p = vec2<f32>(column + 0.5, floor(input.y) + 0.5);
    // The trace clamped to its clip (and to near the screen).
    let top = max(slot.clip_top, -4.0);
    let bottom = min(slot.clip_bottom, globals.size.y + 4.0);
    let s = array<vec4<f32>, 8>(input.s0, input.s1, input.s2, input.s3, input.s4, input.s5,
        input.s6, input.s7);
    // The two slices on either side of the pixel's center: inside either, the pixel is
    // covered fully (in a tall envelope, most are).
    for (var i = SLICES / 2 - 1; i <= SLICES / 2; i++) {
        let v = s[i / 2];
        let range = select(v.xy, v.zw, i % 2 == 1);
        if clamp(range.x, top, bottom) <= p.y && p.y <= clamp(range.y, top, bottom) {
            return input.color;
        }
    }
    let width = 2.0 / f32(SLICES);
    var d = 3.0e38;
    for (var i = 0; i < SLICES; i++) {
        let v = s[i / 2];
        let range = select(v.xy, v.zw, i % 2 == 1);
        if range.x > range.y {
            continue;
        }
        // Distance to the slice's box.
        let x0 = p.x - 1.0 + f32(i) * width;
        let dx = max(max(x0 - p.x, p.x - x0 - width), 0.0);
        let dy = max(max(clamp(range.x, top, bottom) - p.y, p.y - clamp(range.y, top, bottom)), 0.0);
        d = min(d, length(vec2<f32>(dx, dy)));
    }
    // Premultiplied colors scale as a whole.
    return input.color * clamp(1.0 - d, 0.0, 1.0);
}

@vertex
fn vs_hatch(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> Out {
    let hatch = hatches[instance];
    let corner = vec2<f32>(f32(vertex & 1u), f32(vertex >> 1u));
    var out: Out;
    out.position = clip(mix(hatch.rect.xy, hatch.rect.zw, corner));
    out.color = hatch.color;
    out.instance = 0u;
    out.y = 0.0;
    out.s0 = vec4<f32>(0.0);
    out.s1 = vec4<f32>(0.0);
    out.s2 = vec4<f32>(0.0);
    out.s3 = vec4<f32>(0.0);
    out.s4 = vec4<f32>(0.0);
    out.s5 = vec4<f32>(0.0);
    out.s6 = vec4<f32>(0.0);
    out.s7 = vec4<f32>(0.0);
    return out;
}

// Diagonal lines every 6 pixels.
@fragment
fn fs_hatch(input: Out) -> @location(0) vec4<f32> {
    let p = vec2<u32>(input.position.xy);
    if (p.x + p.y) % 6u != 0u {
        discard;
    }
    return input.color;
}
