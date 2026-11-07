//! The window, driven headless through egui_kittest: opening, keys, clicks and the status
//! line, with the trace renderer on a real GPU where there is one.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, Modifiers};
use eframe::egui_wgpu;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use kymo::app::{KymoApp, Options};
use kymo::ui::gpu::Gpu;
use kymo::viewer::Viewer;

/// A generated recording in the temp directory, deleted on drop. Declare it before the
/// harness, so the app (and its memory map) is gone first: Windows refuses to delete
/// mapped files.
struct TempFile(PathBuf);

impl TempFile {
    fn new(extension: &str) -> TempFile {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("kymo-ui-test-{}-{n}.{extension}", std::process::id());
        TempFile(std::env::temp_dir().join(name))
    }

    fn preset(name: &str) -> TempFile {
        TempFile::seeded(name, 0)
    }

    /// A preset with its noise and spikes from `seed` (0 is the preset's own).
    fn seeded(name: &str, seed: u64) -> TempFile {
        let mut spec = kymo_testgen::Spec::preset(name).unwrap();
        spec.seed ^= seed;
        let file = TempFile::new(spec.format.extension());
        kymo_testgen::write(&spec, &file.0).unwrap();
        file
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

const SIZE: egui::Vec2 = egui::vec2(1200.0, 700.0);

/// The window opening `path`, without a GPU: egui skips the trace renderer's callback.
fn window(path: &Path) -> Harness<'static, KymoApp> {
    Harness::builder().with_size(SIZE).build_eframe(|cc| {
        KymoApp::new(
            &cc.egui_ctx,
            Gpu::none("no GPU in this test"),
            Options { sidecar_dir: None },
            Some(path.to_owned()),
        )
    })
}

/// ⌘ on macOS, Ctrl elsewhere, as winit reports them.
fn command() -> Modifiers {
    let base = if cfg!(target_os = "macos") {
        Modifiers::MAC_CMD
    } else {
        Modifiers::CTRL
    };
    base | Modifiers::COMMAND
}

/// Steps until `done` holds, sleeping between steps for the background threads.
fn step_until(harness: &mut Harness<'_, KymoApp>, what: &str, done: impl Fn(&KymoApp) -> bool) {
    let start = Instant::now();
    while !done(harness.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for {what}"
        );
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.step();
}

/// Opens the recording and waits for the pyramid build and every tile of the view.
fn ready(harness: &mut Harness<'_, KymoApp>) {
    step_until(harness, "the recording", |app| app.viewer().is_some());
    step_until(harness, "the build and the tiles", |app| {
        let viewer = app.viewer().unwrap();
        !viewer.status().right.contains("Building") && viewer.complete()
    });
}

#[test]
fn opens_a_recording_and_builds_its_overview() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(&file.0);
    ready(&mut harness);
    let viewer = harness.state().viewer().unwrap();
    assert_eq!(viewer.view().time(), 0.0..20.0);
    assert!(
        viewer.status().right.starts_with("Tiles "),
        "{:?}",
        viewer.status()
    );
    harness.get_by_label("A1");
}

#[test]
fn a_missing_file_shows_the_error() {
    let file = TempFile::new("bdf");
    let mut harness = window(&file.0);
    step_until(&mut harness, "the error", |app| app.error().is_some());
    let error = harness.state().error().unwrap().to_owned();
    assert!(error.contains(&file.0.display().to_string()), "{error}");
    harness.get_by_label("Could not open the recording.");
    harness.get_by_label(&error);
}

#[test]
fn keys_move_the_view() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(&file.0);
    ready(&mut harness);
    let time = |h: &Harness<'_, KymoApp>| h.state().viewer().unwrap().view().time();
    harness.key_press(Key::ArrowRight);
    harness.step();
    assert_eq!(time(&harness), 5.0..25.0);
    let cmd = command();
    harness.key_press_modifiers(cmd, Key::Num9);
    harness.step();
    assert_eq!(time(&harness), 0.0..60.0);
    harness.key_press_modifiers(cmd, Key::Num0);
    harness.step();
    assert_eq!(time(&harness), 0.0..20.0);
    harness.key_press_modifiers(cmd, Key::W);
    harness.step();
    assert!(harness.state().viewer().is_none());
    harness.get_by_label("No recording open.");
}

fn viewer<'h>(harness: &'h Harness<'_, KymoApp>) -> &'h Viewer {
    harness.state().viewer().unwrap()
}

/// Moves the pointer to `pos` and lets the window see it.
fn hover(harness: &mut Harness<'_, KymoApp>, pos: egui::Pos2) {
    harness.hover_at(pos);
    harness.step();
}

/// A primary click at `pos`.
fn click(harness: &mut Harness<'_, KymoApp>, pos: egui::Pos2) {
    hover(harness, pos);
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.step();
}

/// A trackpad scroll of `delta` points at the pointer.
fn scroll(harness: &mut Harness<'_, KymoApp>, delta: egui::Vec2, modifiers: Modifiers) {
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Trackpad,
        modifiers,
    });
    harness.step();
}

#[test]
fn scrolling_pans_time_and_rows() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(&file.0);
    ready(&mut harness);
    // Five rows of five channels, so there is something to scroll.
    harness.key_press_modifiers(command() | Modifiers::SHIFT, Key::Minus);
    harness.step();
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    assert!(rows < 5.0, "{rows} rows");
    hover(&mut harness, layout.traces.center());

    // Content follows the fingers: moving it left shows later times.
    let width = layout.traces.width();
    scroll(&mut harness, egui::vec2(-width / 4.0, 0.0), Modifiers::NONE);
    let time = viewer(&harness).view().time();
    assert!((time.start - 5.0).abs() < 1e-3, "{time:?}");
    // Moving it up by a row shows the next channel.
    let row = layout.traces.height() / rows;
    scroll(&mut harness, egui::vec2(0.0, -row), Modifiers::NONE);
    assert!((viewer(&harness).view().top() - 1.0).abs() < 1e-3);

    // Outside the trace region, scrolling leaves the view alone.
    hover(&mut harness, egui::pos2(5.0, 5.0));
    scroll(&mut harness, egui::vec2(-100.0, -100.0), Modifiers::NONE);
    assert!((viewer(&harness).view().time().start - 5.0).abs() < 1e-3);
    assert!((viewer(&harness).view().top() - 1.0).abs() < 1e-3);
}

#[test]
fn pinching_and_command_scrolling_zoom_around_the_pointer() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(&file.0);
    ready(&mut harness);
    harness.key_press(Key::ArrowRight);
    harness.step();
    let layout = viewer(&harness).layout().unwrap();
    let traces = layout.traces;
    // A quarter of the way in: 5 + 20/4 = 10 s.
    let at = egui::pos2(traces.left() + traces.width() / 4.0, traces.center().y);
    hover(&mut harness, at);
    harness.event(egui::Event::Zoom(2.0));
    harness.step();
    let time = viewer(&harness).view().time();
    assert!((time.end - time.start - 10.0).abs() < 1e-6, "{time:?}");
    assert!(
        (time.start + (time.end - time.start) / 4.0 - 10.0).abs() < 1e-3,
        "{time:?}"
    );

    // ⌘-scrolling zooms out again, around the same time.
    scroll(&mut harness, egui::vec2(0.0, -200.0), command());
    let time = viewer(&harness).view().time();
    assert!((time.end - time.start - 20.0).abs() < 1e-6, "{time:?}");
    assert!((time.start - 5.0).abs() < 1e-3, "{time:?}");
}

#[test]
fn dragging_the_traces_pans() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(&file.0);
    ready(&mut harness);
    harness.key_press(Key::ArrowRight);
    harness.step();
    let traces = viewer(&harness).layout().unwrap().traces;
    let from = traces.center();
    harness.drag_at(from);
    harness.step();
    // Dragging right by a tenth of the width shows 2 s earlier.
    harness.hover_at(from + egui::vec2(traces.width() / 10.0, 0.0));
    harness.step();
    harness.drop_at(from + egui::vec2(traces.width() / 10.0, 0.0));
    harness.step();
    let time = viewer(&harness).view().time();
    assert!((time.start - 3.0).abs() < 1e-3, "{time:?}");
}

#[test]
fn names_and_b_toggle_bad_channels() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(&file.0);
    ready(&mut harness);
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    // Just above the lower edge of the second row, so a row lookup off by the panel's
    // margin lands in the third.
    let row_y = |i: f32| layout.names.top() + (i + 1.0) * layout.names.height() / rows - 2.0;
    let a2 = kymo_core::ChannelId(1);

    let name = egui::pos2(layout.names.center().x, row_y(1.0));
    click(&mut harness, name);
    assert!(viewer(&harness).view().is_bad(a2));

    // B toggles the channel under the pointer, over the traces too.
    hover(
        &mut harness,
        egui::pos2(layout.traces.center().x, row_y(1.0)),
    );
    assert_eq!(viewer(&harness).hover().channel, Some(a2));
    harness.key_press(Key::B);
    harness.step();
    assert!(!viewer(&harness).view().is_bad(a2));
}

#[test]
fn hovering_reads_the_value_under_the_pointer() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(&file.0);
    ready(&mut harness);
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    let y = layout.traces.top() + 0.5 * layout.traces.height() / rows;
    hover(&mut harness, egui::pos2(layout.traces.center().x, y));
    // The tiles may still be arriving; the text settles once they are cached.
    step_until(&mut harness, "a reading", |app| {
        app.viewer().unwrap().status().left.ends_with(" uV")
    });
    let left = viewer(&harness).status().left.clone();
    assert!(left.starts_with("0:10.000 · A1 "), "{left}");
    harness.get_by_label(&left);

    // Off the traces, the time goes away.
    hover(&mut harness, egui::pos2(5.0, 5.0));
    harness.step();
    assert_eq!(viewer(&harness).status().left, "");
}

#[test]
fn the_scrollbars_move_the_view() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(&file.0);
    ready(&mut harness);
    harness.key_press_modifiers(command() | Modifiers::SHIFT, Key::Minus);
    harness.step();
    let layout = viewer(&harness).layout().unwrap();

    // Dragging the time thumb (a third of the track, at its start) to the far end.
    let bar = layout.time_bar;
    let grab = egui::pos2(bar.left() + 5.0, bar.center().y);
    harness.hover_at(grab);
    harness.step();
    harness.drag_at(grab);
    harness.step();
    for x in [bar.center().x, bar.right() + 50.0] {
        harness.hover_at(egui::pos2(x, bar.center().y));
        harness.step();
    }
    harness.drop_at(egui::pos2(bar.right() + 50.0, bar.center().y));
    harness.step();
    assert_eq!(viewer(&harness).view().time(), 40.0..60.0);

    // Clicking the row track below the thumb pages down to the last rows.
    let rows = viewer(&harness).view().rows();
    click(
        &mut harness,
        egui::pos2(layout.row_bar.center().x, layout.row_bar.bottom() - 2.0),
    );
    assert_eq!(viewer(&harness).view().top(), (5 - rows) as f64);
}

#[test]
fn an_idle_window_stops_repainting() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(&file.0);
    ready(&mut harness);
    // Once the build is done and the tiles are in, nothing asks for frames.
    step_until(&mut harness, "the tiles", |app| {
        app.viewer().unwrap().status().right != "Tiles 0 MB"
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(harness.try_run().is_ok(), "the window keeps repainting");
}

/// kittest's wgpu setup on Metal, Vulkan or DX12 (traces need storage buffers in vertex
/// shaders, which OpenGL lacks), or `None` without such an adapter: the test then passes
/// with a message, unless `KYMO_REQUIRE_GPU` is set.
fn gpu_setup() -> Option<egui_wgpu::WgpuSetup> {
    use eframe::egui_wgpu::{self as egui_wgpu, wgpu};
    let descriptor = || {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        desc.backends = wgpu::Backends::PRIMARY;
        desc
    };
    let instance = wgpu::Instance::new(descriptor());
    let found =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .is_ok();
    if !found {
        assert!(
            std::env::var_os("KYMO_REQUIRE_GPU").is_none_or(|v| v.is_empty()),
            "no GPU adapter, and KYMO_REQUIRE_GPU is set"
        );
        eprintln!("skipped: no GPU adapter");
        return None;
    }
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor = descriptor();
    Some(egui_wgpu::WgpuSetup::CreateNew(setup))
}

/// The window opening `path` on a GPU, at one pixel per point.
fn gpu_window(path: &Path, setup: egui_wgpu::WgpuSetup) -> Harness<'static, KymoApp> {
    let path = path.to_owned();
    Harness::builder()
        .with_size(SIZE)
        .wgpu_setup(setup)
        .build_eframe(move |cc| {
            let gpu = Gpu::new(cc.wgpu_render_state.as_ref());
            assert_eq!(gpu.error(), None);
            KymoApp::new(&cc.egui_ctx, gpu, Options { sidecar_dir: None }, Some(path))
        })
}

/// The trace area's pixels.
fn traces_image(harness: &mut Harness<'_, KymoApp>) -> Vec<[u8; 4]> {
    let traces = viewer(harness).layout().unwrap().traces;
    let image = harness.render().unwrap();
    let (x0, y0) = (traces.left() as u32, traces.top() as u32);
    let (x1, y1) = (traces.right() as u32, traces.bottom() as u32);
    (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .map(|(x, y)| image.get_pixel(x, y).0)
        .collect()
}

#[test]
fn opening_another_recording_never_draws_the_last_ones_tiles() {
    let Some(setup) = gpu_setup() else { return };
    // The same layout, so the same tile keys, with different values.
    let first = TempFile::seeded("bdf+c-small", 1);
    let second = TempFile::seeded("bdf+c-small", 2);

    let mut fresh = gpu_window(&second.0, setup.clone());
    ready(&mut fresh);
    let want = traces_image(&mut fresh);

    let mut switched = gpu_window(&first.0, setup);
    ready(&mut switched);
    let before = traces_image(&mut switched);
    assert_ne!(before, want, "the two recordings look the same");
    let ctx = switched.ctx.clone();
    switched.state_mut().open(&ctx, second.0.clone());
    ready(&mut switched);
    assert!(
        traces_image(&mut switched) == want,
        "tiles of the first recording drawn"
    );
}

/// Renders the window on a GPU and writes it to `KYMO_DUMP` (a directory) if set, showing
/// `KYMO_UI_FILE` or a generated recording.
#[test]
fn renders_the_window() {
    if std::env::var_os("KYMO_DUMP").is_none() {
        return;
    }
    let generated = TempFile::preset("edf+d-mixed");
    let path = std::env::var_os("KYMO_UI_FILE").map_or(generated.0.clone(), PathBuf::from);
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .with_pixels_per_point(2.0)
        .wgpu()
        .build_eframe(|cc| {
            KymoApp::new(
                &cc.egui_ctx,
                Gpu::new(cc.wgpu_render_state.as_ref()),
                Options { sidecar_dir: None },
                Some(path.clone()),
            )
        });
    ready(&mut harness);
    for _ in 0..20 {
        harness.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    harness.hover_at(egui::pos2(800.0, 40.0));
    for _ in 0..3 {
        harness.step();
    }
    let image = harness.render().unwrap();
    eprintln!("status: {:?}", harness.state().viewer().unwrap().status());
    let dir = PathBuf::from(std::env::var_os("KYMO_DUMP").unwrap());
    let mut ppm = format!("P6 {} {} 255\n", image.width(), image.height()).into_bytes();
    ppm.extend(image.pixels().flat_map(|p| [p[0], p[1], p[2]]));
    std::fs::write(dir.join("window.ppm"), ppm).unwrap();
}
