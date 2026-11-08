// Trace drawing. One instance per (channel, pixel column) of a tile: it finds the samples
// or buckets that touch its column and draws a one-pixel-wide quad from the highest to the
// lowest row they reach. Quad corners lie on pixel edges, so a peak can never fall between
// pixel centres and vanish.
//
// Positions are physical pixels of the trace area, y down. The CPU side
// (`render/frame.rs`) computes every parameter in f64 relative to an anchor near the
// screen, so the f32 numbers here stay small.

struct Globals {
    size: vec2<f32>,
    _pad: vec2<f32>,
};

// One channel of a tile draw.
struct Slot {
    color: vec4<f32>,
    // The channel's row in the tile.
    row: u32,
    // Native value bits drawn at `offset`.
    reference: u32,
    // Pixels per native unit.
    scale: f32,
    offset: f32,
    // The next tile's first item (a sample twice, or a bucket's minimum and maximum),
    // standing in for item `next_item` of the draw, if `has_next` is 1.
    next_min: u32,
    next_max: u32,
    has_next: u32,
    _pad: u32,
    // The y range (top, bottom) the trace is clamped to: the row for stim channels,
    // everything for others.
    clip_top: f32,
    clip_bottom: f32,
    _pad_clip0: u32,
    _pad_clip1: u32,
};

// One tile draw. Items are samples (level 0) or buckets; item `i` starts at
// `anchor_x + (i - anchor) * item_width`.
struct Draw {
    anchor_x: f32,
    item_width: f32,
    inverse_width: f32,
    anchor: i32,
    first: i32,
    last: i32,
    column0: u32,
    columns: u32,
    slot0: u32,
    // Items per channel in the tile.
    len: u32,
    // 0: int16 (two per word), 1: int32, 2: float32.
    kind: u32,
    // 1 at levels >= 1: a (min, max) pair per bucket.
    pairs: u32,
    // First data word, after the validity bits.
    data: u32,
    // The item that stands for the next tile's first, or 0xffffffff.
    next_item: u32,
    _pad0: u32,
    _pad1: u32,
};

struct Hatch {
    rect: vec4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var<storage, read> slots: array<Slot>;
@group(0) @binding(2) var<uniform> draw: Draw;
@group(0) @binding(3) var<storage, read> hatches: array<Hatch>;
// Validity bits (one per item, shared by all channels), then the values.
@group(1) @binding(0) var<storage, read> tile: array<u32>;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

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
    return out;
}

// Rounded to an integer, kept far from i32 overflow.
fn to_int(x: f32) -> i32 {
    return i32(clamp(x, -1.0e9, 1.0e9));
}

fn valid(item: u32) -> bool {
    return ((tile[item >> 5u] >> (item & 31u)) & 1u) != 0u;
}

// The bits of value `index`, integers sign-extended to 32 bits.
fn bits_at(index: u32) -> u32 {
    if draw.kind == KIND_I16 {
        let word = tile[draw.data + (index >> 1u)];
        let shift = (index & 1u) * 16u;
        // Move the value into the top half, then shift it back down with its sign.
        return bitcast<u32>(bitcast<i32>(word << (16u - shift)) >> 16u);
    }
    return tile[draw.data + index];
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

// y of sample `item` at level 0.
fn sample_y(slot: Slot, item: i32) -> vec2<f32> {
    let i = u32(item);
    if i == draw.next_item {
        if slot.has_next == 0u {
            return vec2<f32>(0.0, 0.0);
        }
        return y_of(slot.next_min, slot);
    }
    if !valid(i) {
        return vec2<f32>(0.0, 0.0);
    }
    return y_of(bits_at(slot.row * draw.len + i), slot);
}

@vertex
fn vs_trace(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> Out {
    let slot = slots[draw.slot0 + instance / draw.columns];
    let column = f32(draw.column0 + instance % draw.columns);
    // The column's edges in items from the anchor. Neighbouring columns compute their shared
    // edge the same way, so an item on it lands in one of them whatever the rounding.
    let xa = column;
    let xb = column + 1.0;
    let ua = (xa - draw.anchor_x) * draw.inverse_width;
    let ub = (xb - draw.anchor_x) * draw.inverse_width;

    var lo = 3.0e38;
    var hi = -3.0e38;
    if draw.pairs == 0u {
        // The polyline through the samples: its vertices inside the column...
        let j0 = max(draw.first, draw.anchor + to_int(ceil(ua)));
        let j1 = min(draw.last, draw.anchor + to_int(floor(ub)));
        for (var j = j0; j <= j1; j++) {
            let y = sample_y(slot, j);
            if y.y > 0.0 {
                lo = min(lo, y.x);
                hi = max(hi, y.x);
            }
        }
        // ...and where it crosses the column's edges.
        for (var e = 0; e < 2; e++) {
            let x = select(xa, xb, e == 1);
            let u = (x - draw.anchor_x) * draw.inverse_width;
            let f = floor(u);
            let j = draw.anchor + to_int(f);
            if j >= draw.first && j < draw.last {
                let a = sample_y(slot, j);
                let b = sample_y(slot, j + 1);
                if a.y > 0.0 && b.y > 0.0 {
                    let y = mix(a.x, b.x, u - f);
                    lo = min(lo, y);
                    hi = max(hi, y);
                }
            }
        }
    } else {
        // Every bucket whose envelope, reaching to the next bucket, touches the column.
        let j0 = max(draw.first, draw.anchor + to_int(ceil(ua - 1.0)));
        let j1 = min(draw.last, draw.anchor + to_int(floor(ub)));
        for (var j = j0; j <= j1; j++) {
            let i = u32(j);
            var a = vec2<f32>(0.0, 0.0);
            var b = vec2<f32>(0.0, 0.0);
            if i == draw.next_item {
                if slot.has_next != 0u {
                    a = y_of(slot.next_min, slot);
                    b = y_of(slot.next_max, slot);
                }
            } else if valid(i) {
                let index = 2u * (slot.row * draw.len + i);
                a = y_of(bits_at(index), slot);
                b = y_of(bits_at(index + 1u), slot);
            }
            if a.y > 0.0 {
                lo = min(lo, a.x);
                hi = max(hi, a.x);
            }
            if b.y > 0.0 {
                lo = min(lo, b.x);
                hi = max(hi, b.x);
            }
        }
    }
    if lo > hi {
        return nothing();
    }
    // A clipped trace runs along the row's edge where it would leave the row.
    lo = clamp(lo, slot.clip_top, slot.clip_bottom);
    hi = clamp(hi, slot.clip_top, slot.clip_bottom);
    // Whole rows: from the row holding the highest point to the one holding the lowest.
    let top = floor(clamp(lo, -2.0, globals.size.y + 2.0));
    let bottom = floor(clamp(hi, -2.0, globals.size.y + 2.0)) + 1.0;
    let corner = vec2<f32>(f32(vertex & 1u), f32(vertex >> 1u));
    var out: Out;
    out.position = clip(vec2<f32>(column + corner.x, mix(top, bottom, corner.y)));
    out.color = slot.color;
    return out;
}

@fragment
fn fs_trace(input: Out) -> @location(0) vec4<f32> {
    return input.color;
}

@vertex
fn vs_hatch(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> Out {
    let hatch = hatches[instance];
    let corner = vec2<f32>(f32(vertex & 1u), f32(vertex >> 1u));
    var out: Out;
    out.position = clip(mix(hatch.rect.xy, hatch.rect.zw, corner));
    out.color = hatch.color;
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
