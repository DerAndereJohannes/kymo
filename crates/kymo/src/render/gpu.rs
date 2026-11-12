//! The GPU side: two pipelines (traces and loading hatches), one storage buffer per tile
//! under a byte budget, and the per-frame parameters. Each tile draw binds its tile and the
//! tiles placed right before and after it, so the shader reads across tile boundaries.

use std::collections::HashMap;
use std::mem::size_of;
use std::num::NonZeroU64;

use bytemuck::{Pod, Zeroable};
use eframe::egui_wgpu::wgpu;
use eframe::egui_wgpu::wgpu::util::DeviceExt as _;
use kymo_core::tile::{Level, Tile, TileData, TileKey};

use super::frame::{Frame, Part};

/// GPU memory for tile buffers, unless set otherwise.
pub const DEFAULT_GPU_BUDGET: usize = 256 << 20;

/// Storage buffers the trace pipeline's stages read: the frame's words (per-channel slots,
/// then item positions of irregular time bases) and three tiles, as wgpu's downlevel
/// limits allow. The hatches have a pipeline of their own.
const VERTEX_STORAGE_BUFFERS: u32 = 4;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RenderError {
    #[error(
        "this graphics device can't draw traces: they need storage buffers in vertex shaders \
         ({needed}, the device has {available}), which Metal, Vulkan and Direct3D 12 provide \
         but OpenGL doesn't"
    )]
    NoVertexStorage { needed: u32, available: u32 },
}

/// Whether a device with `limits` can run the trace renderer. OpenGL devices as eframe
/// creates them (WebGL 2 limits) can't.
pub fn check_support(limits: &wgpu::Limits) -> Result<(), RenderError> {
    let available = limits.max_storage_buffers_per_shader_stage;
    if available < VERTEX_STORAGE_BUFFERS || limits.max_storage_buffer_binding_size == 0 {
        return Err(RenderError::NoVertexStorage {
            needed: VERTEX_STORAGE_BUFFERS,
            available,
        });
    }
    Ok(())
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    size: [f32; 2],
    /// 1 to anti-alias traces, 0 not to.
    antialias: f32,
    /// The word where the item positions start in the frame buffer, after the slots.
    positions: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Slot {
    color: [f32; 4],
    row: u32,
    reference: u32,
    scale: f32,
    offset: f32,
    /// The y range (top, bottom) the trace is clamped to.
    clip: [f32; 2],
    _pad: [u32; 2],
}

/// Where one of a draw's three tiles puts its items, as the shader reads it.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
struct PartParams {
    anchor_x: f32,
    item_width: f32,
    inverse_width: f32,
    anchor: i32,
    /// The items that may be read.
    first: i32,
    last: i32,
    /// Items per channel in the tile.
    len: u32,
    /// 1 at levels >= 1: a (min, max) pair per bucket.
    pairs: u32,
    /// First data word, after the validity bits.
    data: u32,
    present: u32,
    /// For an irregular time base, the first of the part's item positions (counted from
    /// [`Globals::positions`]).
    xs: u32,
    /// [`IRREGULAR`] and [`HOLD`].
    flags: u32,
}

/// The part's items lie at positions from the frame buffer.
const IRREGULAR: u32 = 1;
/// The part's samples hold their values until the next one's start.
const HOLD: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DrawParams {
    /// The tile before, the tile, the tile after.
    parts: [PartParams; 3],
    column0: u32,
    columns: u32,
    slot0: u32,
    /// 0: int16 (two per word), 1: int32, 2: float32.
    kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct HatchRect {
    rect: [f32; 4],
    color: [f32; 4],
}

/// A buffer that grows to fit.
struct Grow {
    buffer: wgpu::Buffer,
    label: &'static str,
    usage: wgpu::BufferUsages,
}

impl Grow {
    const MIN: u64 = 4096;

    fn new(device: &wgpu::Device, label: &'static str, usage: wgpu::BufferUsages) -> Self {
        let usage = usage | wgpu::BufferUsages::COPY_DST;
        Grow {
            buffer: create_buffer(device, label, usage, Self::MIN),
            label,
            usage,
        }
    }

    /// Makes room for `size` bytes; true if that replaced the buffer.
    fn reserve(&mut self, device: &wgpu::Device, size: u64) -> bool {
        if size <= self.buffer.size() {
            return false;
        }
        let size = size.max(2 * self.buffer.size()).next_multiple_of(Self::MIN);
        self.buffer = create_buffer(device, self.label, self.usage, size);
        true
    }

    fn write(&self, queue: &wgpu::Queue, bytes: &[u8]) {
        if !bytes.is_empty() {
            queue.write_buffer(&self.buffer, 0, bytes);
        }
    }
}

fn create_buffer(
    device: &wgpu::Device,
    label: &str,
    usage: wgpu::BufferUsages,
    size: u64,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage,
        mapped_at_creation: false,
    })
}

/// A tile on the GPU.
struct Resident {
    buffer: wgpu::Buffer,
    bytes: usize,
    /// The last frame that drew it.
    used: u64,
}

/// One tile draw, ready to replay.
struct Command {
    offset: u32,
    group: wgpu::BindGroup,
    instances: u32,
}

/// Draws a [`Frame`] into a render pass: `prepare` once per frame, then `paint`.
///
/// Tiles are uploaded the first time they are drawn and stay on the GPU until the budget
/// needs their room; tiles of the current frame are never evicted. Tiles are known by
/// their key, so call [`clear`](Self::clear) before drawing another recording.
pub struct TraceRenderer {
    traces: wgpu::RenderPipeline,
    hatch: wgpu::RenderPipeline,
    frame_layout: wgpu::BindGroupLayout,
    tile_layout: wgpu::BindGroupLayout,
    globals: wgpu::Buffer,
    slots: Grow,
    draws: Grow,
    hatches: Grow,
    frame_group: wgpu::BindGroup,
    hatch_layout: wgpu::BindGroupLayout,
    hatch_group: wgpu::BindGroup,
    draw_stride: u64,
    tiles: HashMap<TileKey, Resident>,
    bytes: usize,
    budget: usize,
    clock: u64,
    commands: Vec<Command>,
    hatch_count: u32,
}

impl TraceRenderer {
    /// A renderer for passes into `format` targets with `samples` samples per pixel and no
    /// depth buffer.
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        samples: u32,
    ) -> Result<Self, RenderError> {
        check_support(&device.limits())?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kymo traces"),
            source: wgpu::ShaderSource::Wgsl(include_str!("traces.wgsl").into()),
        });

        let both = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let buffer =
            |binding, visibility, ty, dynamic, size: Option<u64>| wgpu::BindGroupLayoutEntry {
                binding,
                visibility,
                ty: wgpu::BindingType::Buffer {
                    ty,
                    has_dynamic_offset: dynamic,
                    min_binding_size: size.and_then(NonZeroU64::new),
                },
                count: None,
            };
        let storage = wgpu::BufferBindingType::Storage { read_only: true };
        let uniform = wgpu::BufferBindingType::Uniform;
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kymo frame"),
            entries: &[
                buffer(0, both, uniform, false, Some(size_of::<Globals>() as u64)),
                buffer(1, both, storage, false, None),
                buffer(2, both, uniform, true, Some(size_of::<DrawParams>() as u64)),
            ],
        });
        let hatch_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kymo hatches"),
            entries: &[buffer(3, wgpu::ShaderStages::VERTEX, storage, false, None)],
        });
        let tile_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kymo tile"),
            entries: &[
                buffer(0, both, storage, false, None),
                buffer(1, both, storage, false, None),
                buffer(2, both, storage, false, None),
            ],
        });

        let pipeline = |label, layouts: &[Option<&wgpu::BindGroupLayout>], vs, fs| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: layouts,
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vs),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: samples.max(1),
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let traces = pipeline(
            "kymo traces",
            &[Some(&frame_layout), Some(&tile_layout)],
            "vs_trace",
            "fs_trace",
        );
        let hatch = pipeline(
            "kymo hatch",
            &[Some(&frame_layout), Some(&hatch_layout)],
            "vs_hatch",
            "fs_hatch",
        );

        let globals = create_buffer(
            device,
            "kymo globals",
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            size_of::<Globals>() as u64,
        );
        let slots = Grow::new(device, "kymo slots", wgpu::BufferUsages::STORAGE);
        let draws = Grow::new(device, "kymo draws", wgpu::BufferUsages::UNIFORM);
        let hatches = Grow::new(device, "kymo hatches", wgpu::BufferUsages::STORAGE);
        let frame_group = frame_group(device, &frame_layout, &globals, &slots, &draws);
        let hatch_group = hatch_group(device, &hatch_layout, &hatches);
        let alignment = u64::from(device.limits().min_uniform_buffer_offset_alignment);
        Ok(TraceRenderer {
            traces,
            hatch,
            frame_layout,
            tile_layout,
            globals,
            slots,
            draws,
            hatches,
            frame_group,
            hatch_layout,
            hatch_group,
            draw_stride: (size_of::<DrawParams>() as u64).next_multiple_of(alignment.max(1)),
            tiles: HashMap::new(),
            bytes: 0,
            budget: DEFAULT_GPU_BUDGET,
            clock: 0,
            commands: Vec::new(),
            hatch_count: 0,
        })
    }

    /// GPU memory held by tile buffers.
    pub fn gpu_bytes(&self) -> usize {
        self.bytes
    }

    /// Number of tiles on the GPU.
    pub fn resident_tiles(&self) -> usize {
        self.tiles.len()
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Sets the budget for tile buffers; it applies from the next frame.
    pub fn set_budget(&mut self, bytes: usize) {
        self.budget = bytes;
    }

    /// Drops all tile buffers, for example when another recording opens.
    pub fn clear(&mut self) {
        self.tiles.clear();
        self.bytes = 0;
        self.commands.clear();
        self.hatch_count = 0;
    }

    /// Uploads what `frame` needs and records its draws for [`paint`](Self::paint).
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &Frame) {
        self.clock += 1;
        self.commands.clear();
        let mut slots: Vec<Slot> = Vec::new();
        let mut positions: Vec<f32> = Vec::new();
        let mut params: Vec<u8> = Vec::new();
        for draw in &frame.draws {
            let channels = draw.channels.len() as u32;
            let columns = draw.columns.end.saturating_sub(draw.columns.start);
            if channels == 0 || columns == 0 || draw.part.tile.is_empty() {
                continue;
            }
            let tile = &draw.part.tile;
            let own = self.resident(device, tile);
            let neighbor = |r: &mut Self, part: &Option<Part>, positions: &mut Vec<f32>| {
                part.as_ref()
                    .filter(|p| !p.tile.is_empty())
                    .map(|p| (r.resident(device, &p.tile), part_params(p, positions)))
            };
            let prev = neighbor(self, &draw.prev, &mut positions);
            let next = neighbor(self, &draw.next, &mut positions);
            let buffers = [
                prev.as_ref().map_or(&own, |(b, _)| b),
                &own,
                next.as_ref().map_or(&own, |(b, _)| b),
            ];
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("kymo tiles"),
                layout: &self.tile_layout,
                entries: &[0, 1, 2].map(|i| wgpu::BindGroupEntry {
                    binding: i,
                    resource: buffers[i as usize].as_entire_binding(),
                }),
            });
            let p = DrawParams {
                parts: [
                    prev.map_or_else(PartParams::default, |(_, p)| p),
                    part_params(&draw.part, &mut positions),
                    next.map_or_else(PartParams::default, |(_, p)| p),
                ],
                column0: draw.columns.start,
                columns,
                slot0: slots.len() as u32,
                kind: match tile.data() {
                    TileData::I16(_) => 0,
                    TileData::I32(_) => 1,
                    TileData::F32(_) => 2,
                },
            };
            slots.extend(draw.channels.iter().map(|c| Slot {
                color: c.color,
                row: c.row,
                reference: c.coefficients.reference.bits(),
                scale: c.coefficients.scale,
                offset: c.coefficients.offset,
                clip: c.clip.unwrap_or([f32::MIN, f32::MAX]),
                _pad: [0; 2],
            }));
            let offset = params.len();
            params.extend_from_slice(bytemuck::bytes_of(&p));
            params.resize(offset + self.draw_stride as usize, 0);
            self.commands.push(Command {
                offset: offset as u32,
                group,
                instances: channels * columns,
            });
        }
        self.evict();

        let hatches: Vec<HatchRect> = frame
            .hatches
            .iter()
            .map(|h| HatchRect {
                rect: h.rect,
                color: h.color,
            })
            .collect();
        self.hatch_count = hatches.len() as u32;

        // The slots, then the positions.
        let mut frame_bytes: Vec<u8> = bytemuck::cast_slice(&slots).to_vec();
        let positions_at = (frame_bytes.len() / 4) as u32;
        frame_bytes.extend_from_slice(bytemuck::cast_slice(&positions));
        let hatch_bytes: &[u8] = bytemuck::cast_slice(&hatches);
        let replaced = self.slots.reserve(device, frame_bytes.len() as u64)
            | self.draws.reserve(device, params.len() as u64);
        if replaced {
            self.frame_group = frame_group(
                device,
                &self.frame_layout,
                &self.globals,
                &self.slots,
                &self.draws,
            );
        }
        if self.hatches.reserve(device, hatch_bytes.len() as u64) {
            self.hatch_group = hatch_group(device, &self.hatch_layout, &self.hatches);
        }
        let globals = Globals {
            size: frame.size.map(|s| s as f32),
            antialias: if frame.antialias { 1.0 } else { 0.0 },
            positions: positions_at,
        };
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        self.slots.write(queue, &frame_bytes);
        self.draws.write(queue, &params);
        self.hatches.write(queue, hatch_bytes);
    }

    /// Records the prepared frame into `pass`, whose viewport is the trace area.
    pub fn paint(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.hatch_count > 0 {
            pass.set_pipeline(&self.hatch);
            pass.set_bind_group(0, &self.frame_group, &[0]);
            pass.set_bind_group(1, &self.hatch_group, &[]);
            pass.draw(0..4, 0..self.hatch_count);
        }
        if !self.commands.is_empty() {
            pass.set_pipeline(&self.traces);
        }
        for command in &self.commands {
            pass.set_bind_group(0, &self.frame_group, &[command.offset]);
            pass.set_bind_group(1, &command.group, &[]);
            pass.draw(0..4, 0..command.instances);
        }
    }

    /// `tile`'s buffer, uploading it first if needed.
    fn resident(&mut self, device: &wgpu::Device, tile: &Tile) -> wgpu::Buffer {
        let clock = self.clock;
        if let Some(resident) = self.tiles.get_mut(&tile.key()) {
            resident.used = clock;
            return resident.buffer.clone();
        }
        let words = tile_words(tile);
        let bytes: &[u8] = bytemuck::cast_slice(&words);
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("kymo tile"),
            contents: bytes,
            usage: wgpu::BufferUsages::STORAGE,
        });
        self.bytes += bytes.len();
        self.tiles.insert(
            tile.key(),
            Resident {
                buffer: buffer.clone(),
                bytes: bytes.len(),
                used: clock,
            },
        );
        buffer
    }

    /// Drops the least recently drawn tiles until the budget holds, sparing this frame's.
    fn evict(&mut self) {
        while self.bytes > self.budget {
            let oldest = self
                .tiles
                .iter()
                .filter(|(_, r)| r.used < self.clock)
                .min_by_key(|(_, r)| r.used)
                .map(|(key, _)| *key);
            let Some(key) = oldest else { break };
            if let Some(resident) = self.tiles.remove(&key) {
                self.bytes -= resident.bytes;
            }
        }
    }
}

fn frame_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    globals: &wgpu::Buffer,
    slots: &Grow,
    draws: &Grow,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kymo frame"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: slots.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &draws.buffer,
                    offset: 0,
                    size: NonZeroU64::new(size_of::<DrawParams>() as u64),
                }),
            },
        ],
    })
}

fn hatch_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    hatches: &Grow,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kymo hatches"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 3,
            resource: hatches.buffer.as_entire_binding(),
        }],
    })
}

/// Where `part`'s items lie, for the shader; an irregular part's positions go to the end of
/// `positions`, which follow the slots in the frame buffer.
fn part_params(part: &Part, positions: &mut Vec<f32>) -> PartParams {
    let tile = &part.tile;
    let xs = positions.len() as u32;
    let mut flags = 0;
    if let Some(p) = &part.xs {
        positions.extend_from_slice(p);
        flags |= IRREGULAR;
    }
    if part.hold {
        flags |= HOLD;
    }
    PartParams {
        anchor_x: part.anchor_x,
        item_width: part.item_width,
        inverse_width: part.inverse_width,
        anchor: part.anchor as i32,
        first: *part.items.start() as i32,
        last: *part.items.end() as i32,
        len: tile.len() as u32,
        pairs: u32::from(tile.key().level != Level::RAW),
        data: tile.len().div_ceil(32) as u32,
        present: 1,
        xs,
        flags,
    }
}

/// A tile as the shader reads it: one validity bit per item, then the values (int16
/// packed two per word, low half first).
fn tile_words(tile: &Tile) -> Vec<u32> {
    let len = tile.len();
    let mut words = vec![0u32; len.div_ceil(32)];
    for range in tile.valid() {
        for i in range.start as usize..(range.end as usize).min(len) {
            words[i / 32] |= 1 << (i % 32);
        }
    }
    match tile.data() {
        TileData::I16(d) => words.extend(d.chunks(2).map(|pair| {
            let low = pair[0] as u16;
            let high = pair.get(1).map_or(0, |&v| v as u16);
            u32::from(low) | (u32::from(high) << 16)
        })),
        TileData::I32(d) => words.extend(d.iter().map(|&v| v as u32)),
        TileData::F32(d) => words.extend(d.iter().map(|v| v.to_bits())),
    }
    words
}
