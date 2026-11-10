//! The trace renderer inside egui: it lives in egui-wgpu's callback resources, and each
//! frame hands it a [`Frame`] through a paint callback.

use eframe::egui::{Painter, Rect};
use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait, RenderState, wgpu};

use crate::render::{Frame, TraceRenderer};

/// The window's GPU, if it can draw traces.
pub struct Gpu {
    state: Option<RenderState>,
    /// Why traces can't be drawn.
    error: Option<String>,
}

/// What egui-wgpu keeps for us between frames.
struct Resources {
    renderer: TraceRenderer,
    /// The recording the renderer's tiles belong to (see [`Gpu::paint`]).
    serial: u64,
}

impl Gpu {
    /// Sets up the trace renderer on eframe's device; without one (another backend, a
    /// test harness) traces aren't drawn.
    pub fn new(state: Option<&RenderState>) -> Gpu {
        let Some(state) = state else {
            return Gpu::none("Traces need a wgpu device.");
        };
        match TraceRenderer::new(&state.device, state.target_format, 1) {
            Ok(renderer) => {
                state.renderer.write().callback_resources.insert(Resources {
                    renderer,
                    serial: 0,
                });
                Gpu {
                    state: Some(state.clone()),
                    error: None,
                }
            }
            Err(error) => {
                let info = state.adapter.get_info();
                Gpu::none(&format!(
                    "Cannot draw traces on {} ({:?}): {error}.",
                    info.name, info.backend
                ))
            }
        }
    }

    /// No GPU, for `reason`.
    pub fn none(reason: &str) -> Gpu {
        Gpu {
            state: None,
            error: Some(reason.to_owned()),
        }
    }

    /// Why traces can't be drawn, if they can't.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The budget for tiles on the GPU.
    pub fn budget(&self) -> Option<usize> {
        let state = self.state.as_ref()?;
        let renderer = state.renderer.read();
        let resources = renderer.callback_resources.get::<Resources>()?;
        Some(resources.renderer.budget())
    }

    /// Sets the budget for tiles on the GPU; tiles beyond it go at the next frame.
    pub fn set_budget(&self, bytes: usize) {
        if let Some(state) = &self.state {
            let mut renderer = state.renderer.write();
            if let Some(resources) = renderer.callback_resources.get_mut::<Resources>() {
                resources.renderer.set_budget(bytes);
            }
        }
    }

    /// Paints `frame` into `rect`, which must lie on whole physical pixels and measure
    /// `frame.size` of them. `serial` names the recording: tile keys don't, so the
    /// renderer drops its tiles when it changes.
    pub fn paint(&self, painter: &Painter, rect: Rect, frame: Frame, serial: u64) {
        if self.state.is_some() {
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                Traces { frame, serial },
            ));
        }
    }
}

struct Traces {
    frame: Frame,
    serial: u64,
}

impl CallbackTrait for Traces {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(resources) = resources.get_mut::<Resources>() {
            if resources.serial != self.serial {
                resources.renderer.clear();
                resources.serial = self.serial;
            }
            resources.renderer.prepare(device, queue, &self.frame);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: eframe::egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        if let Some(resources) = resources.get::<Resources>() {
            resources.renderer.paint(pass);
        }
    }
}
