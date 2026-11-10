//! The window, driven headless through egui_kittest: opening, keys, clicks and the status
//! line, with the trace renderer on a real GPU where there is one.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, Modifiers};
use eframe::egui_wgpu;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use kymo::action::Action;
use kymo::annotations::{AnnotationColor, AnnotationColors};
use kymo::app::{KymoApp, Menus, Options};
use kymo::app_mode::AppMode;
use kymo::keymap::Platform;
use kymo::settings::{Settings, Theme, load_annotation_colors};
use kymo::ui::gpu::Gpu;
use kymo::ui::palette::{annotation_color, trace_color};
use kymo::view::{KINDS, kind_index};
use kymo::viewer::Viewer;
use kymo_core::ChannelKind;
use kymo_testgen::{TempDir, TempFile};

/// A preset with its noise and spikes from `seed` (0 is the preset's own).
fn seeded(name: &str, seed: u64) -> TempFile {
    let mut spec = kymo_testgen::Spec::preset(name).unwrap();
    spec.seed ^= seed;
    TempFile::written(&spec)
}

/// A preset without its trigger channel, so only the file's annotations are shown.
fn without_triggers(name: &str) -> TempFile {
    let mut spec = kymo_testgen::Spec::preset(name).unwrap();
    spec.channels.retain(|c| !c.trigger);
    TempFile::written(&spec)
}

/// Pyramids in memory, the menu bar drawn by egui as on Linux, on every platform, and
/// default settings.
fn options() -> Options {
    Options {
        sidecar_dir: None,
        menus: Menus::Egui(Platform::Linux),
        settings: Settings::default(),
        annotation_colors: Default::default(),
    }
}

/// The system font fallback eframe adds (for ⌘ and friends), as the app has it.
fn system_fonts(ctx: &egui::Context) {
    ctx.add_font_provider(std::sync::Arc::new(
        egui_system_font_provider::SystemFontProvider::new(),
    ));
}

const SIZE: egui::Vec2 = egui::vec2(1200.0, 700.0);

/// The window opening `path`, without a GPU: egui skips the trace renderer's callback.
fn window(path: &Path) -> Harness<'static, KymoApp> {
    window_from(Harness::builder(), path)
}

/// [`window`], its screenshots rendered on the shared GPU (see [`shared_gpu`]) rather than
/// on a device kittest creates for it; on kittest's where there is no shared one.
fn screenshot_window(path: &Path) -> Harness<'static, KymoApp> {
    let mut builder = Harness::builder();
    if let Some(gpu) = shared_gpu() {
        builder = builder.wgpu_setup(egui_wgpu::WgpuSetup::Existing(gpu.clone()));
    }
    window_from(builder, path)
}

fn window_from(
    builder: egui_kittest::HarnessBuilder<KymoApp>,
    path: &Path,
) -> Harness<'static, KymoApp> {
    builder.with_size(SIZE).build_eframe(|cc| {
        KymoApp::new(
            &cc.egui_ctx,
            Gpu::none("no GPU in this test"),
            options(),
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

/// Whether the status line reads a value in µV (EEG's unit, whatever the file says); the
/// annotations under the pointer may follow it.
fn has_reading(status: &str) -> bool {
    status.split(" · ").any(|part| part.ends_with(" \u{b5}V"))
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
    let mut harness = window(file.path());
    ready(&mut harness);
    let viewer = harness.state().viewer().unwrap();
    assert_eq!(viewer.view().time(), 0.0..20.0);
    // Once the overview is built, the status line's right side is empty.
    assert_eq!(viewer.status().right, "", "{:?}", viewer.status());
    harness.get_by_label("A1");
}

#[test]
fn a_missing_file_shows_the_error() {
    let file = TempFile::new("bdf");
    let mut harness = window(file.path());
    step_until(&mut harness, "the error", |app| app.error().is_some());
    let error = harness.state().error().unwrap().to_owned();
    assert!(
        error.contains(&file.path().display().to_string()),
        "{error}"
    );
    harness.get_by_label("Could not open the recording.");
    harness.get_by_label(&error);
}

#[test]
fn keys_move_the_view() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
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
    let mut harness = window(file.path());
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
    let mut harness = window(file.path());
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
    let mut harness = window(file.path());
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
fn clicking_a_name_toggles_bad() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
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
    click(&mut harness, name);
    assert!(!viewer(&harness).view().is_bad(a2));
}

/// The channel list shows units as the readout does: µV for EEG, mV for sEEG, whatever the
/// file stores (uV for both here).
#[test]
fn the_channel_list_shows_display_units() {
    use egui::accesskit::Role;
    let file = TempFile::preset("edf+c-kinds");
    let mut harness = window(file.path());
    ready(&mut harness);
    // The list draws the rows in view: EEG Fz and SEEG LA1 come first.
    let unit_of = |name: &str| {
        let y = harness
            .get_by_role_and_label(Role::Label, name)
            .rect()
            .center()
            .y;
        ["\u{b5}V", "mV", "uV"].into_iter().find(|unit| {
            harness
                .query_all_by_role_and_label(Role::Label, unit)
                .any(|n| (n.rect().center().y - y).abs() < 1.0)
        })
    };
    assert_eq!(unit_of("EEG Fz"), Some("\u{b5}V"));
    assert_eq!(unit_of("SEEG LA1"), Some("mV"));
    assert!(harness.query_by_label("uV").is_none());
}

#[test]
fn hovering_reads_the_value_under_the_pointer() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    let y = layout.traces.top() + 0.5 * layout.traces.height() / rows;
    hover(&mut harness, egui::pos2(layout.traces.center().x, y));
    // The tiles may still be arriving; the text settles once they are cached.
    step_until(&mut harness, "a reading", |app| {
        has_reading(&app.viewer().unwrap().status().left)
    });
    let left = viewer(&harness).status().left.clone();
    assert!(left.starts_with("0:10.000 · A1 "), "{left}");
    // The trigger that starts at 10 s (Status, channel 4, at 512 Hz).
    let spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    let code = spec.trigger_code(4, 5120).unwrap();
    assert_ne!(spec.trigger_code(4, 5119), Some(code));
    assert!(left.ends_with(&format!(" \u{b5}V · {code}")), "{left}");
    harness.get_by_label(&left);

    // Off the traces, the time goes away.
    hover(&mut harness, egui::pos2(5.0, 5.0));
    harness.step();
    assert_eq!(viewer(&harness).status().left, "");
}

#[test]
fn the_scrollbars_move_the_view() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
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
    let mut harness = window(file.path());
    ready(&mut harness);
    // Once the build is done and the tiles are in, nothing asks for frames.
    step_until(&mut harness, "the tiles", |app| {
        app.viewer().unwrap().status().right != "Tiles 0 MB"
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(harness.try_run().is_ok(), "the window keeps repainting");
}

/// Clicks `item` in the egui menu `menu`.
fn choose(harness: &mut Harness<'_, KymoApp>, menu: &str, item: &str) {
    harness.get_by_label(menu).click();
    harness.run_steps(2);
    // Items with a shortcut carry it in their label.
    harness.get_by_label_contains(item).click();
    harness.run_steps(2);
}

/// Whether `item` in the egui menu `menu` is enabled; leaves the menu closed.
fn enabled(harness: &mut Harness<'_, KymoApp>, menu: &str, item: &str) -> bool {
    harness.get_by_label(menu).click();
    harness.run_steps(2);
    let enabled = !harness
        .get_by_label_contains(item)
        .accesskit_node()
        .is_disabled();
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(
        harness.query_by_label_contains(item).is_none(),
        "{menu} stayed open"
    );
    enabled
}

#[test]
fn menu_items_are_enabled_as_the_state_allows() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    assert!(enabled(&mut harness, "View", "Zoom In"));
    assert!(enabled(&mut harness, "File", "Close"));
    assert!(enabled(&mut harness, "View", "Show Sidebar"));
    assert!(enabled(&mut harness, "View", "Remove DC"));
    assert!(enabled(&mut harness, "Edit", "Settings"));

    harness.key_press_modifiers(command(), Key::W);
    harness.step();
    assert!(!enabled(&mut harness, "File", "Close"));
    assert!(!enabled(&mut harness, "View", "Zoom In"));
    assert!(!enabled(&mut harness, "View", "Show Sidebar"));
    assert!(enabled(&mut harness, "File", "Quit"));
    assert!(enabled(&mut harness, "Help", "Keyboard Shortcuts"));
    assert!(enabled(&mut harness, "Edit", "Settings"));
}

#[test]
fn menu_items_act_and_disabled_ones_dont() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    choose(&mut harness, "View", "Zoom In");
    assert_eq!(viewer(&harness).view().time(), 0.0..10.0);
    choose(&mut harness, "Go", "End of Recording");
    assert_eq!(viewer(&harness).view().time(), 50.0..60.0);
    choose(&mut harness, "View", "Remove DC");
    assert!(viewer(&harness).view().dc_removed());
    choose(&mut harness, "Edit", "Settings");
    assert!(harness.state().settings_open());
    harness.get_by_label("Tile cache");
    // Its close button.
    harness.get_by_label("Close window").click();
    harness.run_steps(2);
    assert!(!harness.state().settings_open());

    choose(&mut harness, "File", "Close");
    assert!(harness.state().viewer().is_none());
    // Nothing to close now.
    choose(&mut harness, "File", "Close");
    harness.get_by_label("No recording open.");
    assert_eq!(harness.state().error(), None);
}

/// Whether the checkable `item` in the egui menu `menu` is checked; leaves the menu closed.
fn checked(harness: &mut Harness<'_, KymoApp>, menu: &str, item: &str) -> bool {
    harness.get_by_label(menu).click();
    harness.run_steps(2);
    let checked = harness
        .get_by_label_contains(item)
        .accesskit_node()
        .toggled()
        .unwrap()
        == egui::accesskit::Toggled::True;
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    checked
}

#[test]
fn checkable_menu_items_show_a_checkbox_or_radio_button() {
    // Show Sidebar on, Anti-aliasing off (the settings' default).
    let file = TempFile::preset("bdf+c-small");
    let mut harness = screenshot_window(file.path());
    ready(&mut harness);
    harness.get_by_label("View").click();
    harness.run_steps(2);
    let item = |harness: &Harness<'_, KymoApp>, label: &str| {
        let node = harness.get_by_label_contains(label);
        let node = node.accesskit_node();
        (
            node.role(),
            node.toggled(),
            harness.get_by_label_contains(label).rect(),
        )
    };
    let (sidebar_role, sidebar, sidebar_rect) = item(&harness, "Show Sidebar");
    let (antialias_role, antialias, antialias_rect) = item(&harness, "Anti-aliasing");
    use egui::accesskit::{Role, Toggled};
    assert_eq!(
        (sidebar_role, sidebar),
        (Role::CheckBox, Some(Toggled::True))
    );
    assert_eq!(
        (antialias_role, antialias),
        (Role::CheckBox, Some(Toggled::False))
    );

    // Drawn as a box at the start, in the checkboxes' fill, and without the selection's
    // background, which marked them before.
    let image = harness.render().unwrap();
    let pixel = |x: f32, y: f32| image.get_pixel(x as u32, y as u32).0;
    let style = harness.ctx.global_style();
    let size = style.spacing.icon_width;
    let box_center = |r: egui::Rect| pixel(r.left() + 2.0 + size / 2.0, r.center().y);
    assert_eq!(
        box_center(antialias_rect),
        style.visuals.widgets.inactive.bg_fill.to_array()
    );
    let selection = style.visuals.selection.bg_fill.to_array();
    for r in [sidebar_rect, antialias_rect] {
        assert_ne!(pixel(r.right() - 1.0, r.center().y), selection);
    }
    assert_eq!(
        pixel(sidebar_rect.right() - 1.0, sidebar_rect.center().y),
        pixel(antialias_rect.right() - 1.0, antialias_rect.center().y)
    );

    // The stim annotation rules exclude each other, so they are radio buttons, with the
    // default rule on; the initial code is a checkbox of its own.
    harness.get_by_label("Stim Annotations").hover();
    harness.run_steps(2);
    let rules = ["Every Change", "From Zero", "Increasing"].map(|label| {
        let (role, on, _) = item(&harness, label);
        (role, on)
    });
    assert_eq!(
        rules,
        [
            (Role::RadioButton, Some(Toggled::True)),
            (Role::RadioButton, Some(Toggled::False)),
            (Role::RadioButton, Some(Toggled::False)),
        ]
    );
    assert_eq!(item(&harness, "Initial Code").0, Role::CheckBox);
}

/// A preset with every channel's physical range moved by `offset`, as amplifiers with a
/// DC offset record it (BioSemi files sit tens of millivolts off zero).
fn offset_preset(name: &str, offset: f64) -> TempFile {
    let mut spec = kymo_testgen::Spec::preset(name).unwrap();
    for channel in spec.channels.iter_mut().filter(|c| !c.trigger) {
        channel.physical_min += offset;
        channel.physical_max += offset;
    }
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    file
}

/// The mean of `channel` over the samples of `time`, brute force.
fn window_mean(
    rec: &kymo_core::Recording,
    channel: kymo_core::ChannelId,
    time: std::ops::Range<f64>,
) -> Option<f64> {
    let grid_id = rec.channel(channel).grid;
    let grid = rec.grid(grid_id);
    let range = kymo_core::tile::samples_in(grid, &time);
    let (mut sum, mut n) = (0.0, 0usize);
    for part in grid.present_in(range) {
        let mut out = vec![0.0f32; (part.end - part.start) as usize];
        rec.read_physical(grid_id, &[channel], part, &mut out)
            .unwrap();
        sum += out.iter().map(|&x| f64::from(x)).sum::<f64>();
        n += out.len();
    }
    (n > 0).then(|| sum / n as f64)
}

/// Whether every visible channel's baseline is its brute-force mean over the view (0 for
/// stim channels).
fn baselines_are_window_means(app: &KymoApp) -> bool {
    let viewer = app.viewer().unwrap();
    let view = viewer.view();
    let rec = &viewer.document().recording;
    view.visible_channels().iter().all(|&c| {
        // DC removal leaves stim channels alone.
        let want = if rec.channel(c).kind == kymo_core::ChannelKind::Stim {
            0.0
        } else {
            window_mean(rec, c, view.time()).unwrap()
        };
        // The brute force reads f32 values.
        (viewer.baseline(c) - want).abs() <= 1e-5 * want.abs().max(1.0)
    })
}

/// The numbers of the status line's reading ("12.5 µV" or "−3 … 4 µV").
fn reading_numbers(status: &str) -> Vec<f64> {
    let reading = status.rsplit_once(" A1 ").unwrap().1;
    // Up to the annotations under the pointer, if any.
    let reading = reading.split(" · ").next().unwrap();
    reading
        .trim_end_matches(" \u{b5}V")
        .split(" … ")
        .map(|x| x.replace('−', "-").parse().unwrap())
        .collect()
}

#[test]
fn dc_removal_centers_each_trace_on_its_window_mean() {
    let file = offset_preset("bdf+c-small", -21_000.0);
    let mut harness = window(file.path());
    ready(&mut harness);
    let a1 = kymo_core::ChannelId(0);
    assert_eq!(viewer(&harness).baseline(a1), 0.0);
    assert!(!checked(&mut harness, "View", "Remove DC"));

    // The value under the pointer, without and with DC removal.
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    let a1_row = egui::pos2(
        layout.traces.center().x,
        layout.traces.top() + 0.5 * layout.traces.height() / rows,
    );
    hover(&mut harness, a1_row);
    step_until(&mut harness, "a reading", |app| {
        has_reading(&app.viewer().unwrap().status().left)
    });
    let raw = reading_numbers(&viewer(&harness).status().left);
    assert!(raw.iter().all(|&x| x < -20_000.0), "{raw:?}");

    harness.key_press_modifiers(command(), Key::D);
    harness.step();
    assert!(viewer(&harness).view().dc_removed());
    step_until(&mut harness, "the means", baselines_are_window_means);
    let mean = viewer(&harness).baseline(a1);
    assert!((-21_100.0..-20_900.0).contains(&mean), "{mean}");
    let centered = reading_numbers(&viewer(&harness).status().left);
    assert_eq!(centered.len(), raw.len());
    for (r, c) in raw.iter().zip(&centered) {
        // Four significant digits of the raw value: within 10 uV.
        assert!((r - mean - c).abs() <= 10.0, "{r} − {mean} ≠ {c}");
    }
    assert!(checked(&mut harness, "View", "Remove DC"));

    // The means follow the window.
    harness.key_press(Key::ArrowRight);
    harness.step();
    step_until(&mut harness, "the panned means", baselines_are_window_means);
    assert_eq!(viewer(&harness).view().time(), 5.0..25.0);

    harness.key_press_modifiers(command(), Key::D);
    harness.step();
    assert_eq!(viewer(&harness).baseline(a1), 0.0);
    assert!(!checked(&mut harness, "View", "Remove DC"));
}

/// The numbers of the status line's reading of the Status channel, if it shows one. Codes
/// have no unit (the file's is "Boolean"); the annotations under the pointer may follow.
fn status_reading(status: &str) -> Option<Vec<f64>> {
    let reading = status.rsplit_once(" Status ")?.1;
    // Up to the annotations under the pointer, if any.
    let reading = reading.split(" · ").next()?;
    Some(
        reading
            .split(" … ")
            .map(|x| x.replace('−', "-").parse().unwrap())
            .collect(),
    )
}

#[test]
fn stim_channels_read_their_codes_with_and_without_dc_removal() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let status = kymo_core::ChannelId(4);
    assert_eq!(
        viewer(&harness).document().recording.channel(status).name,
        "Status"
    );
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    let row = viewer(&harness).view().row_of(status).unwrap() as f32;
    let y = layout.traces.top() + (row + 0.5) * layout.traces.height() / rows;
    // Readings across the window: the codes 0 to 255, without the status bits above them.
    let readings = |harness: &mut Harness<'_, KymoApp>| {
        (1..20)
            .map(|k| {
                let x = layout.traces.left() + layout.traces.width() * k as f32 / 20.0;
                hover(harness, egui::pos2(x, y));
                step_until(harness, "a reading", |app| {
                    status_reading(&app.viewer().unwrap().status().left).is_some()
                });
                status_reading(&viewer(harness).status().left).unwrap()
            })
            .collect::<Vec<_>>()
    };
    let raw = readings(&mut harness);
    let numbers: Vec<f64> = raw.iter().flatten().copied().collect();
    assert!(
        numbers
            .iter()
            .all(|&x| (0.0..=255.0).contains(&x) && x.fract() == 0.0),
        "{raw:?}"
    );
    assert!(numbers.iter().any(|&x| x > 0.0), "{raw:?}");

    // DC removal leaves the codes as they are.
    harness.key_press_modifiers(command(), Key::D);
    harness.step();
    step_until(&mut harness, "the means", baselines_are_window_means);
    assert_eq!(viewer(&harness).baseline(status), 0.0);
    assert_ne!(viewer(&harness).baseline(kymo_core::ChannelId(0)), 0.0);
    assert_eq!(readings(&mut harness), raw);
}

/// bdf+c-small with its channels relabeled to five types: A1 and A2 (EEG), "EOG L",
/// "ECG" and Status (stim).
fn typed_preset() -> TempFile {
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.channels[2].label = "EOG L".into();
    spec.channels[3].label = "ECG".into();
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    file
}

/// Clicks the sidebar widget labeled `label` (exactly, with `role`).
fn click_in_list(harness: &mut Harness<'_, KymoApp>, role: egui::accesskit::Role, label: &str) {
    // A row scrolled out of its list takes no clicks. Scrolled to at once, without
    // the animation, which would leave it moving under the pointer.
    harness
        .ctx
        .global_style_mut(|s| s.scroll_animation = egui::style::ScrollAnimation::none());
    harness.get_by_role_and_label(role, label).scroll_to_me();
    // One frame scrolls, the next lays the rows out there, the third hit-tests them.
    harness.run_steps(3);
    harness.get_by_role_and_label(role, label).click();
    harness.run_steps(2);
}

fn toggled(
    harness: &Harness<'_, KymoApp>,
    role: egui::accesskit::Role,
    label: &str,
) -> egui::accesskit::Toggled {
    harness
        .get_by_role_and_label(role, label)
        .accesskit_node()
        .toggled()
        .unwrap()
}

fn shown(harness: &Harness<'_, KymoApp>) -> Vec<u32> {
    viewer(harness)
        .view()
        .visible_channels()
        .iter()
        .map(|c| c.0)
        .collect()
}

#[test]
fn the_channel_list_shows_hides_and_marks_bad() {
    use egui::accesskit::{Role, Toggled};
    let file = typed_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    assert_eq!(shown(&harness), [0, 1, 2, 3, 4]);
    for kind in ["EEG", "EOG", "ECG", "Stim"] {
        assert_eq!(
            toggled(&harness, Role::CheckBox, kind),
            Toggled::True,
            "{kind}"
        );
    }
    assert!(
        harness
            .query_by_role_and_label(Role::CheckBox, "EMG")
            .is_none()
    );
    harness.get_by_label("2 of 2 shown");

    // One channel's box hides it; its type's box is mixed then.
    click_in_list(&mut harness, Role::CheckBox, "Show A2");
    assert_eq!(shown(&harness), [0, 2, 3, 4]);
    assert_eq!(toggled(&harness, Role::CheckBox, "Show A2"), Toggled::False);
    assert_eq!(toggled(&harness, Role::CheckBox, "EEG"), Toggled::Mixed);
    harness.get_by_label("1 of 2 shown");
    // A mixed type's box shows them all, and then hides them all.
    click_in_list(&mut harness, Role::CheckBox, "EEG");
    assert_eq!(shown(&harness), [0, 1, 2, 3, 4]);
    click_in_list(&mut harness, Role::CheckBox, "EEG");
    assert_eq!(shown(&harness), [2, 3, 4]);
    assert_eq!(toggled(&harness, Role::CheckBox, "Show A1"), Toggled::False);
    assert_eq!(toggled(&harness, Role::CheckBox, "EEG"), Toggled::False);
    click_in_list(&mut harness, Role::CheckBox, "EEG");
    assert_eq!(shown(&harness), [0, 1, 2, 3, 4]);

    // The bad toggle.
    let ecg = kymo_core::ChannelId(3);
    click_in_list(&mut harness, Role::Button, "ECG bad");
    assert!(viewer(&harness).view().is_bad(ecg));
    assert_eq!(toggled(&harness, Role::Button, "ECG bad"), Toggled::True);
    click_in_list(&mut harness, Role::Button, "ECG bad");
    assert!(!viewer(&harness).view().is_bad(ecg));

    // A click on a name scrolls the traces to it: three rows of five.
    for _ in 0..2 {
        harness.key_press_modifiers(command() | Modifiers::SHIFT, Key::Minus);
        harness.step();
    }
    assert_eq!(viewer(&harness).view().rows(), 3);
    click_in_list(&mut harness, Role::Label, "EOG L");
    assert_eq!(viewer(&harness).view().top(), 2.0);
    click_in_list(&mut harness, Role::Label, "A2");
    assert_eq!(viewer(&harness).view().top(), 1.0);
    // A hidden channel's name doesn't scroll.
    click_in_list(&mut harness, Role::CheckBox, "Show A1");
    click_in_list(&mut harness, Role::Label, "A1");
    assert_eq!(shown(&harness), [1, 2, 3]);
}

/// The left edges of the sidebar list's labels `label`; panics unless there are some and
/// they are all the same.
fn column_left(harness: &Harness<'_, KymoApp>, role: egui::accesskit::Role, label: &str) -> f32 {
    let lefts: Vec<f32> = harness
        .query_all_by_role_and_label(role, label)
        .map(|n| n.rect().left())
        .collect();
    assert!(!lefts.is_empty(), "no {label}");
    assert!(lefts.iter().all(|&x| x == lefts[0]), "{label}: {lefts:?}");
    lefts[0]
}

#[test]
fn the_channel_list_keeps_its_columns_while_scrolling() {
    use egui::accesskit::Role;
    // 60 channels: only the last EEG channel has a long name, and Status comes after it.
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    let status = spec.channels.pop().unwrap();
    for k in 0..55 {
        let label = if k == 54 {
            "LONGCHANNELNAME1".to_owned()
        } else {
            format!("E{k}")
        };
        spec.channels
            .push(kymo_testgen::ChannelSpec::eeg(&label, 512, spec.format));
    }
    spec.channels.push(status);
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    let mut harness = window(file.path());
    ready(&mut harness);
    assert!(harness.query_by_label("LONGCHANNELNAME1").is_none());
    let bad = harness.get_by_label("A1 bad").rect().left();
    let eeg = column_left(&harness, Role::Label, "EEG");
    let uv = column_left(&harness, Role::Label, "\u{b5}V");

    // Scrolled to the end, where the long name and Status are.
    let a1 = harness
        .get_by_role_and_label(Role::Label, "A1")
        .rect()
        .center();
    harness.hover_at(a1);
    for _ in 0..10 {
        scroll(&mut harness, egui::vec2(0.0, -2000.0), Modifiers::NONE);
    }
    harness.run_steps(2);
    let long = harness.get_by_label("LONGCHANNELNAME1").rect();
    assert_eq!(
        harness.get_by_label("LONGCHANNELNAME1 bad").rect().left(),
        bad
    );
    assert_eq!(harness.get_by_label("Status bad").rect().left(), bad);
    assert!(
        long.right() < bad,
        "{long:?} reaches the bad toggles at {bad}"
    );
    assert_eq!(column_left(&harness, Role::Label, "EEG"), eeg);
    assert_eq!(column_left(&harness, Role::Label, "\u{b5}V"), uv);
    // Codes have no unit, whatever the file says.
    assert!(harness.query_by_label("Boolean").is_none());
}

/// Rates of different lengths ("64 Hz", "256 Hz") end at the same edge; an EDF stim channel
/// shows no unit either.
#[test]
fn the_channel_list_right_aligns_rates() {
    use egui::accesskit::Role;
    let file = TempFile::preset("edf+d-mixed");
    let mut harness = window(file.path());
    ready(&mut harness);
    let rights: Vec<(String, f32)> = ["64 Hz", "256 Hz", "512 Hz"]
        .iter()
        .flat_map(|rate| {
            harness
                .query_all_by_role_and_label(Role::Label, rate)
                .map(|n| (rate.to_string(), n.rect().right()))
                .collect::<Vec<_>>()
        })
        .collect();
    // The rows in view: each rate at least once.
    for rate in ["64 Hz", "256 Hz", "512 Hz"] {
        assert!(rights.iter().any(|r| r.0 == rate), "{rights:?}");
    }
    assert!(rights.iter().all(|r| r.1 == rights[0].1), "{rights:?}");
    let lefts: Vec<f32> = ["64 Hz", "256 Hz"]
        .iter()
        .map(|rate| harness.get_all_by_label(rate).next().unwrap().rect().left())
        .collect();
    assert!(lefts[0] > lefts[1], "{lefts:?}");

    // Scrolled to the end, where the trigger channel is.
    let first = harness
        .get_by_role_and_label(Role::Label, "EEG Fz")
        .rect()
        .center();
    harness.hover_at(first);
    scroll(&mut harness, egui::vec2(0.0, -2000.0), Modifiers::NONE);
    harness.run_steps(2);
    assert!(harness.query_by_label("TRIGGER").is_some());
    assert!(harness.query_by_label("Boolean").is_none());
}

/// A rate that isn't round shows two decimals in the channel list and the summary; hovering
/// the summary's shows it exactly, with the channels and samples.
#[test]
fn rates_are_rounded_with_the_exact_one_on_hover() {
    use egui::accesskit::Role;
    // As MNELAB exported a recording: 500.0065 Hz.
    let mut spec = kymo_testgen::Spec::preset("bv-int16-multiplexed").unwrap();
    let spr = spec.channels[0].samples_per_record;
    spec.record_duration = f64::from(spr) / 500.006_492_535_299_46;
    // Its markers sit on samples at 500 Hz, not at this rate.
    spec.annotations.clear();
    let file = TempFile::written(&spec);
    let mut harness = window(file.path());
    ready(&mut harness);
    let rate = viewer(&harness).document().recording.grids()[0].rate();
    assert_eq!(format!("{rate:.4}"), "500.0065");
    // Every channel's row and the summary's Sampling row.
    let rounded = harness
        .query_all_by_role_and_label(Role::Label, "500.01 Hz")
        .count();
    assert_eq!(rounded, spec.channels.len() + 1);
    let exact = format!("{rate} Hz: {} channels, ", spec.channels.len());
    assert!(harness.query_by_label_contains(&exact).is_none());
    let sampling = harness
        .query_all_by_role_and_label(Role::Label, "500.01 Hz")
        .map(|n| n.rect())
        .min_by(|a, b| a.top().total_cmp(&b.top()))
        .unwrap();
    // Tooltips wait for the pointer to rest.
    hover(&mut harness, sampling.center());
    let shown = Instant::now() + Duration::from_secs(2);
    while harness.query_by_label_contains(&exact).is_none() && Instant::now() < shown {
        harness.step();
    }
    harness.get_by_label_contains(&exact);
}

/// The legend lists every description with its count; its checkboxes show and hide
/// annotations by description, or all at once.
#[test]
fn the_legend_shows_and_hides_annotations() {
    use egui::accesskit::{Role, Toggled};
    // In the first 20 s: "event 0" (an event, color 0), "sleep stage N2" (2.5 s, color 1),
    // and "eyes closed" and "Elektrodenprüfung µV" (events at 15 s, colors 2 and 3; one
    // label, as the second would overlap the first).
    let file = without_triggers("edf+d-mixed");
    let mut harness = window(file.path());
    ready(&mut harness);
    // 20 "event k", 7 "stim k", and three more.
    assert_eq!(viewer(&harness).legend().shown_count(), (30, 30));
    let colors = |harness: &Harness<'_, KymoApp>| {
        let marks = viewer(harness).marks();
        let mut bands: Vec<_> = marks.bands.iter().map(|m| m.color).collect();
        let mut lines: Vec<_> = marks.lines.iter().map(|m| m.color).collect();
        bands.sort();
        lines.sort();
        (bands, lines, marks.labels.len())
    };
    use AnnotationColor::Palette;
    assert_eq!(
        colors(&harness),
        (
            vec![Palette(1)],
            vec![Palette(0), Palette(2), Palette(3)],
            3
        )
    );

    // The legend is at the bottom of the sidebar, below the types and channels, and shows
    // 8 rows before it scrolls.
    let top = |harness: &Harness<'_, KymoApp>, label: &str| {
        harness
            .get_by_role_and_label(Role::CheckBox, label)
            .rect()
            .top()
    };
    let legend = top(&harness, "Annotations");
    let names: Vec<String> = viewer(&harness)
        .document()
        .recording
        .channels()
        .iter()
        .map(|c| format!("Show {}", c.name))
        .collect();
    let channels: Vec<f32> = names
        .iter()
        .filter_map(|n| harness.query_by_role_and_label(Role::CheckBox, n))
        .map(|n| n.rect().top())
        .collect();
    assert!(!channels.is_empty());
    assert!(top(&harness, "EEG") < channels[0], "{channels:?}");
    assert!(channels[0] < legend, "{channels:?} {legend}");
    let row = harness.ctx.global_style().spacing.interact_size.y;
    // The reading under the pointer marks the status line.
    let traces = viewer(&harness).layout().unwrap().traces;
    hover(&mut harness, traces.center());
    harness.step();
    let status = harness
        .get_by_label(&viewer(&harness).status().left)
        .rect()
        .top();
    assert!(
        status - legend > 9.0 * row,
        "legend at {legend}, status at {status}"
    );

    assert!(harness.query_by_label("sleep stage N2").is_some());
    assert!(harness.query_by_label("Elektrodenprüfung µV").is_some());
    assert_eq!(
        viewer(&harness)
            .legend()
            .entry("sleep stage N2")
            .unwrap()
            .count,
        20
    );
    // The Annotations row counts them all, right-aligned with the descriptions' counts.
    let in_row = |harness: &Harness<'_, KymoApp>, checkbox: &str, text: &str| {
        let row = harness
            .get_by_role_and_label(Role::CheckBox, checkbox)
            .rect();
        let found: Vec<_> = harness
            .query_all_by_label(text)
            .map(|n| n.rect())
            .filter(|r| row.y_range().contains(r.center().y))
            .collect();
        assert_eq!(found.len(), 1, "{text} in the row of {checkbox}");
        found[0]
    };
    let total = viewer(&harness).document().annotations.len();
    let total = in_row(&harness, "Annotations", &total.to_string());
    let count = in_row(&harness, "Show sleep stage N2", "20");
    assert!(
        (total.right() - count.right()).abs() < 0.5,
        "{total} {count}"
    );

    click_in_list(&mut harness, Role::CheckBox, "Show sleep stage N2");
    assert_eq!(
        colors(&harness),
        (vec![], vec![Palette(0), Palette(2), Palette(3)], 2)
    );
    assert_eq!(viewer(&harness).legend().shown_count(), (29, 30));
    assert_eq!(
        toggled(&harness, Role::CheckBox, "Annotations"),
        Toggled::Mixed
    );

    // A mixed box shows them all; then it hides them all.
    click_in_list(&mut harness, Role::CheckBox, "Annotations");
    assert_eq!(
        colors(&harness),
        (
            vec![Palette(1)],
            vec![Palette(0), Palette(2), Palette(3)],
            3
        )
    );
    click_in_list(&mut harness, Role::CheckBox, "Annotations");
    assert_eq!(colors(&harness), (vec![], vec![], 0));
    assert_eq!(
        toggled(&harness, Role::CheckBox, "Show event 0"),
        Toggled::False
    );
}

/// The status line names the shown annotations under the pointer.
#[test]
fn the_status_line_names_the_annotations_under_the_pointer() {
    // "sleep stage N2" from 7.625 s to 10.125 s; "eyes closed" and "Elektrodenprüfung µV",
    // events at 15 s.
    let file = without_triggers("edf+d-mixed");
    let mut harness = window(file.path());
    ready(&mut harness);
    let traces = viewer(&harness).layout().unwrap().traces;
    let at = |harness: &Harness<'_, KymoApp>, t: f64| {
        let x = viewer(harness).view().x_at(t, f64::from(traces.width()));
        egui::pos2(traces.left() + x as f32, traces.top() + 20.0)
    };
    let status = |harness: &Harness<'_, KymoApp>| viewer(harness).status().left.clone();

    let p = at(&harness, 9.0);
    hover(&mut harness, p);
    assert!(
        status(&harness).ends_with(" · sleep stage N2"),
        "{}",
        status(&harness)
    );
    let p = at(&harness, 12.0);
    hover(&mut harness, p);
    assert!(!status(&harness).contains("N2"), "{}", status(&harness));
    // An event line is hit from a point away.
    let p = at(&harness, 15.0) + egui::vec2(1.0, 0.0);
    hover(&mut harness, p);
    assert!(
        status(&harness).ends_with(" · eyes closed, Elektrodenprüfung µV"),
        "{}",
        status(&harness)
    );

    // Hidden annotations aren't named.
    click_in_list(
        &mut harness,
        egui::accesskit::Role::CheckBox,
        "Show eyes closed",
    );
    hover(&mut harness, p);
    assert!(
        status(&harness).ends_with(" · Elektrodenprüfung µV"),
        "{}",
        status(&harness)
    );

    // Off the traces, they go away with the time.
    hover(&mut harness, egui::pos2(5.0, 5.0));
    assert_eq!(status(&harness), "");
}

/// Without annotations or stim channels there is no legend.
#[test]
fn recordings_without_annotations_have_no_legend() {
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.annotations.clear();
    spec.channels.retain(|c| !c.trigger);
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    let mut harness = window(file.path());
    ready(&mut harness);
    let role = egui::accesskit::Role::CheckBox;
    assert!(
        harness
            .query_by_role_and_label(role, "Annotations")
            .is_none()
    );
    assert_eq!(
        viewer(&harness).marks(),
        &kymo::annotations::Marks::default()
    );
}

/// The stim annotations of `spec`'s Status channel (channel 4, 512 Hz), brute force from the
/// codes it was written with: onsets and codes, in time order, where `event(before, now)`
/// says whether a change is one.
fn expected_stim(
    spec: &kymo_testgen::Spec,
    event: fn(i32, i32) -> bool,
    initial: bool,
) -> Vec<(f64, String)> {
    let mut events = Vec::new();
    let mut before = None;
    for i in 0..60 * 512 {
        let code = spec.trigger_code(4, i).unwrap();
        let is_event = match before {
            None => initial && code != 0,
            Some(b) => b != code && code != 0 && event(b, code),
        };
        if is_event {
            events.push((i as f64 / 512.0, code.to_string()));
        }
        before = Some(code);
    }
    events
}

/// The viewer's stim annotations, onsets and codes; each must belong to Status alone and last
/// no time.
fn stim_annotations(harness: &Harness<'_, KymoApp>) -> Vec<(f64, String)> {
    viewer(harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin == kymo_core::Origin::Stim)
        .map(|a| {
            assert_eq!(a.data.channels, [kymo_core::ChannelId(4)]);
            assert_eq!(a.data.duration, 0.0);
            (a.data.onset, a.data.description.clone())
        })
        .collect()
}

fn assert_stim(got: &[(f64, String)], want: &[(f64, String)]) {
    assert_eq!(got.len(), want.len(), "{got:?}\n{want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!(
            (g.0 - w.0).abs() < 1e-9 && g.1 == w.1,
            "{g:?} instead of {w:?}"
        );
    }
}

fn file_annotations(harness: &Harness<'_, KymoApp>) -> Vec<kymo_core::Annotation> {
    viewer(harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin == kymo_core::Origin::File)
        .cloned()
        .collect()
}

/// Opens View → Stim Annotations.
fn open_stim_menu(harness: &mut Harness<'_, KymoApp>) {
    harness.get_by_label("View").click();
    harness.run_steps(2);
    harness.get_by_label("Stim Annotations").click();
    harness.run_steps(2);
}

/// Whether `item` in View → Stim Annotations is enabled and checked; leaves the menus closed.
fn stim_item(harness: &mut Harness<'_, KymoApp>, item: &str) -> (bool, bool) {
    open_stim_menu(harness);
    let node = harness.get_by_label(item);
    let enabled = !node.accesskit_node().is_disabled();
    let checked = node.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True);
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(
        harness.query_by_label(item).is_none(),
        "the menu stayed open"
    );
    (enabled, checked)
}

fn choose_stim(harness: &mut Harness<'_, KymoApp>, item: &str) {
    open_stim_menu(harness);
    harness.get_by_label(item).click();
    harness.run_steps(2);
}

/// Whether a stim channel's code changes to a non-zero one are events, by rule.
fn every_change(_: i32, _: i32) -> bool {
    true
}
fn from_zero(before: i32, _: i32) -> bool {
    before == 0
}
fn increasing(before: i32, now: i32) -> bool {
    now > before
}

#[test]
fn stim_annotations_follow_the_rule_and_keep_the_file_annotations() {
    use egui::accesskit::Role;
    // A seed whose first code is not 0, so Initial Code makes a difference.
    let seed = (0..)
        .find(|&seed| {
            let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
            spec.seed ^= seed;
            spec.trigger_code(4, 0) != Some(0)
        })
        .unwrap();
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.seed ^= seed;
    let file = seeded("bdf+c-small", seed);
    let mut harness = window(file.path());
    ready(&mut harness);

    let want = expected_stim(&spec, every_change, false);
    assert!(want.len() > 20, "{} events", want.len());
    assert_stim(&stim_annotations(&harness), &want);
    assert_eq!(stim_item(&mut harness, "Every Change"), (true, true));
    assert_eq!(stim_item(&mut harness, "Initial Code"), (true, false));
    // The legend lists the codes after the file's descriptions, all shown.
    let legend = viewer(&harness).legend();
    let codes: std::collections::BTreeSet<&str> = want.iter().map(|w| w.1.as_str()).collect();
    for code in &codes {
        let entry = legend.entry(code).unwrap();
        assert!(entry.shown && entry.count > 0, "{entry:?}");
    }
    assert_eq!(legend.shown_count(), (12 + codes.len(), 12 + codes.len()));

    // The file's annotations and the legend's choices stay through rule changes.
    click_in_list(&mut harness, Role::CheckBox, "Show trigger 0");
    let files = file_annotations(&harness);
    assert_eq!(files.len(), 12);
    for (item, event) in [
        ("From Zero", from_zero as fn(i32, i32) -> bool),
        ("Increasing", increasing),
    ] {
        choose_stim(&mut harness, item);
        assert_stim(
            &stim_annotations(&harness),
            &expected_stim(&spec, event, false),
        );
        assert_eq!(stim_item(&mut harness, item), (true, true));
        assert_eq!(stim_item(&mut harness, "Every Change"), (true, false));
        assert_eq!(file_annotations(&harness), files);
        assert!(!viewer(&harness).legend().entry("trigger 0").unwrap().shown);
    }
    choose_stim(&mut harness, "Initial Code");
    let initial = expected_stim(&spec, increasing, true);
    assert_eq!(initial[0].0, 0.0);
    assert_stim(&stim_annotations(&harness), &initial);
    assert_eq!(stim_item(&mut harness, "Initial Code"), (true, true));

    // The recording opened next keeps the rule.
    let ctx = harness.ctx.clone();
    harness.state_mut().open(&ctx, file.path().to_owned());
    ready(&mut harness);
    assert_stim(&stim_annotations(&harness), &initial);
    assert_eq!(stim_item(&mut harness, "Increasing"), (true, true));
}

#[test]
fn stim_stim_items_need_stim_channels() {
    let file = without_triggers("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    assert!(stim_annotations(&harness).is_empty());
    for item in ["Every Change", "From Zero", "Increasing", "Initial Code"] {
        assert!(!stim_item(&mut harness, item).0, "{item}");
    }
}

/// A reopened recording has its events from its sidecar in its first frame.
#[test]
fn stim_annotations_come_with_the_sidecar() {
    let file = TempFile::preset("bdf+c-small");
    let dir = TempDir::new();
    let open = |path: &Path| {
        let path = path.to_owned();
        let sidecars = dir.path().to_owned();
        Harness::builder().with_size(SIZE).build_eframe(move |cc| {
            KymoApp::new(
                &cc.egui_ctx,
                Gpu::none("no GPU in this test"),
                Options {
                    sidecar_dir: Some(sidecars),
                    ..options()
                },
                Some(path),
            )
        })
    };
    let spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    let want = expected_stim(&spec, every_change, false);
    {
        let mut harness = open(file.path());
        ready(&mut harness);
        assert_stim(&stim_annotations(&harness), &want);
    }
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "no sidecar"
    );

    let mut harness = open(file.path());
    step_until(&mut harness, "the recording", |app| app.viewer().is_some());
    assert!(!viewer(&harness).status().right.contains("Building"));
    assert_stim(&stim_annotations(&harness), &want);
    drop(harness);
}

#[test]
fn the_sidebar_hides_and_shows() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let width = |h: &Harness<'_, KymoApp>| viewer(h).layout().unwrap().traces.width();
    let with_sidebar = width(&harness);
    harness.get_by_label("Format");
    choose(&mut harness, "View", "Show Sidebar");
    assert!(!harness.state().sidebar_shown());
    assert!(harness.query_by_label("Format").is_none());
    assert!(width(&harness) > with_sidebar + 100.0);
    choose(&mut harness, "View", "Show Sidebar");
    harness.get_by_label("Format");
    assert_eq!(width(&harness), with_sidebar);
}

/// The window opening `path` with `settings`, without a GPU.
fn window_with(path: &Path, settings: Settings) -> Harness<'static, KymoApp> {
    let path = path.to_owned();
    Harness::builder().with_size(SIZE).build_eframe(move |cc| {
        KymoApp::new(
            &cc.egui_ctx,
            Gpu::none("no GPU in this test"),
            Options {
                settings,
                ..options()
            },
            Some(path),
        )
    })
}

/// Sets the number field labeled `label` in the Settings window.
fn set_field(harness: &mut Harness<'_, KymoApp>, label: &str, value: f64) {
    harness
        .get_by_role_and_label(egui::accesskit::Role::SpinButton, label)
        .set_value_accesskit(value);
    harness.run_steps(2);
}

#[test]
fn settings_apply_at_once() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.key_press_modifiers(command(), Key::Comma);
    harness.step();
    assert!(harness.state().settings_open());

    // The tile cache: the service's budget changes, and the cached tiles stay within it.
    set_field(&mut harness, "Tile cache", 100.0);
    assert_eq!(harness.state().settings().tile_cache_mb, 100);
    let cache = viewer(&harness).cache();
    assert_eq!(cache.budget(), 100 << 20);
    assert!(cache.bytes() <= cache.budget());
    // Out of range: the nearest allowed value.
    set_field(&mut harness, "Tile cache", 1.0);
    assert_eq!(viewer(&harness).cache().budget(), 64 << 20);

    // The default window: ⌘0 shows it from now on; the window on screen stays.
    set_field(&mut harness, "Default window", 12.5);
    assert_eq!(viewer(&harness).view().time(), 0.0..20.0);
    harness.key_press(Key::ArrowRight);
    harness.key_press_modifiers(command(), Key::Num0);
    harness.step();
    assert_eq!(viewer(&harness).view().time(), 5.0..17.5);

    set_field(&mut harness, "GPU memory", 512.0);
    assert_eq!(harness.state().settings().gpu_memory_mb, 512);

    // Restore Defaults, at once too.
    harness.get_by_label("Restore Defaults").click();
    harness.run_steps(2);
    assert_eq!(*harness.state().settings(), Settings::default());
    assert_eq!(viewer(&harness).cache().budget(), 256 << 20);
    harness.key_press_modifiers(command(), Key::Num0);
    harness.step();
    assert_eq!(viewer(&harness).view().time(), 5.0..25.0);
}

#[test]
fn anti_aliasing_follows_the_menu_settings_and_shortcut() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    // Off by default.
    assert!(!harness.state().settings().antialias);
    assert!(!checked(&mut harness, "View", "Anti-aliasing"));
    choose(&mut harness, "View", "Anti-aliasing");
    assert!(harness.state().settings().antialias);
    assert!(checked(&mut harness, "View", "Anti-aliasing"));

    // The Settings checkbox shows and changes the same setting.
    harness.key_press_modifiers(command(), Key::Comma);
    harness.step();
    let checkbox = harness.get_by_label("Anti-aliased");
    assert_eq!(
        checkbox.accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::True)
    );
    checkbox.click();
    harness.run_steps(2);
    assert!(!harness.state().settings().antialias);
    assert!(!checked(&mut harness, "View", "Anti-aliasing"));
    harness.get_by_label("Close window").click();
    harness.run_steps(2);

    // ⇧⌘A switches it too.
    harness.key_press_modifiers(command() | Modifiers::SHIFT, Key::A);
    harness.step();
    assert!(harness.state().settings().antialias);

    // A setting, so it can change without a recording.
    harness.key_press_modifiers(command(), Key::W);
    harness.step();
    assert!(enabled(&mut harness, "View", "Anti-aliasing"));
    choose(&mut harness, "View", "Anti-aliasing");
    assert!(!harness.state().settings().antialias);
}

/// Each type's amplitude per channel spacing, in [`KINDS`] order.
fn amplitudes(harness: &Harness<'_, KymoApp>) -> [f64; KINDS.len()] {
    let view = viewer(harness).view();
    KINDS.map(|kind| view.amplitude(kind))
}

#[test]
fn auto_scale_fits_the_window_from_the_menu_keys_and_setting() {
    let file = typed_preset();
    // Not on opening, which is the default: the fixed defaults first, then ⌘R.
    let mut harness = window_with(
        file.path(),
        Settings {
            auto_scale_on_open: false,
            ..Settings::default()
        },
    );
    ready(&mut harness);
    let defaults = amplitudes(&harness);

    // ⌘R: the generated signals reach 15 uV, their spikes (to the digital limits) are rare.
    harness.key_press_modifiers(command(), Key::R);
    harness.run_steps(2);
    let fitted = amplitudes(&harness);
    let signal = kymo_testgen::EEG_AMPLITUDE * 1e-6;
    for kind in [ChannelKind::Eeg, ChannelKind::Eog, ChannelKind::Ecg] {
        let amplitude = fitted[kind_index(kind)];
        assert!(
            (signal / 4.0..=signal * 2.0).contains(&amplitude),
            "{kind:?}: {amplitude}"
        );
    }
    // Stim keeps its scale, and so do types without channels.
    for kind in [ChannelKind::Stim, ChannelKind::Emg, ChannelKind::Misc] {
        assert_eq!(
            fitted[kind_index(kind)],
            defaults[kind_index(kind)],
            "{kind:?}"
        );
    }

    // Once fitted, the scale stays as the view moves on.
    harness.key_press(Key::ArrowRight);
    harness.run_steps(2);
    step_until(&mut harness, "the tiles", |app| {
        app.viewer().unwrap().complete()
    });
    assert_eq!(amplitudes(&harness), fitted);
    harness.key_press(Key::ArrowLeft);
    harness.step();

    // Default Scale (⌥⌘0) brings the defaults back; the menu item fits again.
    harness.key_press_modifiers(command() | Modifiers::ALT, Key::Num0);
    harness.step();
    assert_eq!(amplitudes(&harness), defaults);
    choose(&mut harness, "View", "Auto Scale");
    harness.run_steps(2);
    assert_eq!(amplitudes(&harness), fitted);
    assert!(enabled(&mut harness, "View", "Default Scale"));

    // The setting: a recording opens fitted, as ⌘R fits it.
    let settings = Settings {
        auto_scale_on_open: true,
        ..Settings::default()
    };
    let mut harness = window_with(file.path(), settings);
    ready(&mut harness);
    assert_eq!(amplitudes(&harness), fitted);

    // The Settings checkbox shows and changes it.
    harness.key_press_modifiers(command(), Key::Comma);
    harness.step();
    harness.get_by_label("Automatic on opening").click();
    harness.run_steps(2);
    assert!(!harness.state().settings().auto_scale_on_open);
}

#[test]
fn auto_scale_waits_for_the_windows_tiles() {
    // Long enough that its pyramid takes a while: 16 channels at 1000 Hz, 30 min (58 MB).
    let mut spec = kymo_testgen::Spec::preset("edf-64ch-1h").unwrap();
    spec.channels.truncate(16);
    spec.n_records = 1800;
    let file = TempFile::written(&spec);
    let mut harness = window(file.path());
    // One frame per step, so the view is still the first one when the viewer appears.
    while harness.state().viewer().is_none() {
        harness.step();
    }
    let defaults = amplitudes(&harness);
    // The whole recording, from the pyramid's top levels, which the build makes last; Fit
    // and Auto Scale in one frame, so the tiles can't arrive between them.
    let input = harness.input_mut();
    for key in [Key::Num9, Key::R] {
        input.events.push(egui::Event::Key {
            key,
            pressed: true,
            modifiers: command(),
            repeat: false,
            physical_key: None,
        });
    }
    let mut incomplete = 0;
    loop {
        harness.step();
        if viewer(&harness).complete() {
            break;
        }
        assert_eq!(amplitudes(&harness), defaults);
        incomplete += 1;
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(incomplete > 0, "the whole recording had its tiles at once");
    assert_eq!(
        viewer(&harness).view().time(),
        0.0..viewer(&harness).view().duration()
    );
    let fitted = amplitudes(&harness);
    assert_ne!(fitted, defaults);
    // What it fits then is what the complete window gives.
    harness.key_press_modifiers(command() | Modifiers::ALT, Key::Num0);
    harness.key_press_modifiers(command(), Key::R);
    harness.run_steps(2);
    assert_eq!(amplitudes(&harness), fitted);
}

#[test]
fn auto_scale_ignores_a_dc_offset() {
    let eeg = kind_index(ChannelKind::Eeg);
    let mut fitted = Vec::new();
    for offset in [0.0, 21_000.0] {
        let file = offset_preset("bdf+c-small", offset);
        let mut harness = window_with(
            file.path(),
            Settings {
                auto_scale_on_open: false,
                ..Settings::default()
            },
        );
        ready(&mut harness);
        let default = amplitudes(&harness)[eeg];
        harness.key_press_modifiers(command(), Key::R);
        harness.run_steps(2);
        fitted.push(amplitudes(&harness)[eeg]);
        assert_ne!(fitted.last(), Some(&default));
    }
    assert!(
        (fitted[0] - fitted[1]).abs() <= fitted[0] * 1e-6,
        "{fitted:?}"
    );
}

/// eframe's storage, in memory.
#[derive(Default)]
struct Memory(std::collections::HashMap<String, String>);

impl eframe::Storage for Memory {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }
    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }
    fn flush(&mut self) {}
}

/// The window opening `path` with `options`, with the system fonts the app has: egui's
/// color picker draws a 📋.
fn fonts_window(path: &Path, options: Options) -> Harness<'static, KymoApp> {
    let path = path.to_owned();
    Harness::builder()
        .with_size(SIZE)
        .with_context_setup(system_fonts)
        .build_eframe(move |cc| {
            KymoApp::new(
                &cc.egui_ctx,
                Gpu::none("no GPU in this test"),
                options,
                Some(path),
            )
        })
}

/// Opens the color picker of the swatch labeled `label` and picks `rgb` in it.
fn pick(harness: &mut Harness<'_, KymoApp>, label: &str, rgb: [u8; 3]) {
    if harness.query_by_label("Reset").is_none() {
        harness.get_by_label(label).click();
        harness.run_steps(2);
    }
    for (channel, value) in ["R", "G", "B"].into_iter().zip(rgb) {
        harness
            .get_by_role_and_label(egui::accesskit::Role::SpinButton, channel)
            .set_value_accesskit(f64::from(value));
        harness.run_steps(2);
    }
}

#[test]
fn channel_type_colors_are_picked_and_reset() {
    let file = typed_preset();
    let mut harness = fonts_window(file.path(), options());
    ready(&mut harness);
    let picked = |h: &Harness<'_, KymoApp>| h.state().settings().type_colors;
    let eog = kind_index(ChannelKind::Eog);
    harness.key_press_modifiers(command(), Key::Comma);
    harness.step();

    // The picker starts at the type's color; Reset is off while it is the default.
    harness.get_by_label("Color of EOG").click();
    harness.run_steps(2);
    let visuals = harness.ctx.global_style().visuals.clone();
    let default = trace_color(ChannelKind::Eog, &visuals);
    let value = |h: &Harness<'_, KymoApp>, channel: &str| {
        h.get_by_role_and_label(egui::accesskit::Role::SpinButton, channel)
            .accesskit_node()
            .numeric_value()
    };
    assert_eq!(value(&harness, "B"), Some(f64::from(default.b())));
    assert!(harness.get_by_label("Reset").accesskit_node().is_disabled());

    pick(&mut harness, "Color of EOG", [10, 200, 30]);
    let mut want = [None; KINDS.len()];
    want[eog] = Some(egui::Color32::from_rgb(10, 200, 30));
    assert_eq!(picked(&harness), want);
    harness.get_by_label("Reset").click();
    harness.run_steps(2);
    assert_eq!(picked(&harness), [None; KINDS.len()]);

    // Restore Defaults resets them too.
    pick(&mut harness, "Color of EOG", [10, 200, 30]);
    assert_eq!(picked(&harness), want);
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    harness.get_by_label("Restore Defaults").click();
    harness.run_steps(2);
    assert_eq!(picked(&harness), [None; KINDS.len()]);
}

#[test]
fn annotation_colors_are_picked_per_description_and_kept() {
    use AnnotationColor::{Custom, Palette};
    let file = without_triggers("edf+d-mixed");
    let mut harness = fonts_window(file.path(), options());
    ready(&mut harness);
    let color = |h: &Harness<'_, KymoApp>, d: &str| viewer(h).legend().entry(d).unwrap().color;
    let line_colors = |h: &Harness<'_, KymoApp>| {
        let mut colors: Vec<_> = viewer(h).marks().lines.iter().map(|m| m.color).collect();
        colors.sort();
        colors
    };
    let green = Custom([10, 200, 30]);
    assert_eq!(line_colors(&harness), [Palette(0), Palette(2), Palette(3)]);

    // "event 0" (an event at the start) turns green; the others keep their colors.
    pick(&mut harness, "Color of event 0", [10, 200, 30]);
    assert_eq!(
        harness.state().annotation_colors(),
        &AnnotationColors::from([("event 0".to_owned(), [10, 200, 30])])
    );
    assert_eq!(color(&harness, "event 0"), green);
    assert_eq!(color(&harness, "sleep stage N2"), Palette(1));
    assert_eq!(line_colors(&harness), [Palette(2), Palette(3), green]);

    // Reset: the palette color again.
    harness.get_by_label("Reset").click();
    harness.run_steps(2);
    assert!(harness.state().annotation_colors().is_empty());
    assert_eq!(color(&harness, "event 0"), Palette(0));
    assert_eq!(line_colors(&harness), [Palette(0), Palette(2), Palette(3)]);

    // Kept for the next run, and for every recording with the description.
    pick(&mut harness, "Color of event 0", [10, 200, 30]);
    let mut storage = Memory::default();
    eframe::App::save(harness.state_mut(), &mut storage);
    let other = TempFile::preset("edf+d-mixed");
    let options = Options {
        annotation_colors: load_annotation_colors(Some(&storage)),
        ..options()
    };
    let mut harness = fonts_window(other.path(), options);
    ready(&mut harness);
    assert_eq!(color(&harness, "event 0"), green);
}

#[test]
fn settings_are_kept_between_runs() {
    let file = TempFile::preset("bdf+c-small");
    let mut storage = Memory::default();
    {
        let mut harness = window(file.path());
        ready(&mut harness);
        harness.key_press_modifiers(command(), Key::Comma);
        harness.step();
        set_field(&mut harness, "Default window", 8.0);
        set_field(&mut harness, "Tile cache", 300.0);
        eframe::App::save(harness.state_mut(), &mut storage);
    }

    // The next run starts with them: a recording opens with an 8 s window and a 300 MB
    // cache.
    let settings = Settings::load(Some(&storage));
    assert_eq!(settings.default_window_s, 8.0);
    assert_eq!(settings.tile_cache_mb, 300);
    let mut harness = window_with(file.path(), settings);
    ready(&mut harness);
    assert_eq!(viewer(&harness).view().time(), 0.0..8.0);
    assert_eq!(viewer(&harness).cache().budget(), 300 << 20);
    assert!(!harness.state().settings_open());
}

#[test]
fn the_shortcut_overview_and_about_open() {
    let file = TempFile::new("bdf");
    // A missing file: both work without a recording.
    let mut harness = window(file.path());
    step_until(&mut harness, "the error", |app| app.error().is_some());
    harness.key_press_modifiers(command(), Key::Slash);
    harness.step();
    assert!(harness.state().shortcuts_open());
    harness.get_by_label("Trackpad and Mouse");
    harness.get_by_label("Scroll channels");
    harness.key_press_modifiers(command(), Key::Slash);
    harness.step();
    assert!(!harness.state().shortcuts_open());
    assert!(harness.query_by_label("Scroll channels").is_none());

    choose(&mut harness, "Help", "About Kymo");
    assert!(harness.state().about_open());
    harness.get_by_label(kymo::ui::menus::ABOUT);
}

#[test]
fn esc_and_close_close_the_window_in_front() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let open = |h: &Harness<'_, KymoApp>| (h.state().settings_open(), h.state().shortcuts_open());
    let press = |h: &mut Harness<'_, KymoApp>, modifiers, key| {
        h.key_press_modifiers(modifiers, key);
        h.run_steps(2);
    };

    // The one opened last is in front.
    press(&mut harness, command(), Key::Comma);
    press(&mut harness, command(), Key::Slash);
    assert_eq!(open(&harness), (true, true));
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(open(&harness), (true, false));
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(open(&harness), (false, false));

    // A click brings one to the front.
    press(&mut harness, command(), Key::Slash);
    press(&mut harness, command(), Key::Comma);
    harness.get_by_label("Pan time").click();
    harness.run_steps(2);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(open(&harness), (true, false));

    // ⌘W closes the window in front, and with none the recording.
    press(&mut harness, command(), Key::W);
    assert_eq!(open(&harness), (false, false));
    assert!(harness.state().viewer().is_some());
    press(&mut harness, command(), Key::W);
    assert!(harness.state().viewer().is_none());

    // Without a recording, ⌘W still closes Settings.
    press(&mut harness, command(), Key::Comma);
    assert!(harness.state().settings_open());
    press(&mut harness, command(), Key::W);
    assert!(!harness.state().settings_open());
}

/// The point in the traces at time `t`, `dy` points below their top.
fn at_time(harness: &Harness<'_, KymoApp>, t: f64, dy: f32) -> egui::Pos2 {
    let traces = viewer(harness).layout().unwrap().traces;
    let x = viewer(harness).view().x_at(t, f64::from(traces.width()));
    egui::pos2(traces.left() + x as f32, traces.top() + dy)
}

/// A click in the traces at time `t`, near their top.
fn click_at_time(harness: &mut Harness<'_, KymoApp>, t: f64) {
    let p = at_time(harness, t, 20.0);
    click(harness, p);
}

/// A primary drag from `from` to `to`, through the middle.
fn drag(harness: &mut Harness<'_, KymoApp>, from: egui::Pos2, to: egui::Pos2) {
    hover(harness, from);
    harness.drag_at(from);
    harness.step();
    for pos in [from + (to - from) / 2.0, to] {
        hover(harness, pos);
    }
    harness.drop_at(to);
    harness.step();
}

/// The annotations made in the viewer: onset, duration and description.
fn made(harness: &Harness<'_, KymoApp>) -> Vec<(f64, f64, String)> {
    viewer(harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin == kymo_core::Origin::Viewer)
        .inspect(|a| assert!(a.data.channels.is_empty()))
        .map(|a| (a.data.onset, a.data.duration, a.data.description.clone()))
        .collect()
}

fn press(harness: &mut Harness<'_, KymoApp>, modifiers: Modifiers, key: Key) {
    harness.key_press_modifiers(modifiers, key);
    harness.step();
}

/// Types `text` into the description of new annotations, in place of what it held,
/// deselecting first: with a selection, the field edits the selected annotation's.
fn describe(harness: &mut Harness<'_, KymoApp>, text: &str) {
    press(harness, Modifiers::NONE, Key::Escape);
    let field = "Description of new annotations";
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    press(harness, command(), Key::A);
    harness.get_by_label(field).type_text(text);
    harness.run_steps(2);
}

fn selected_data(harness: &Harness<'_, KymoApp>) -> Option<kymo_core::AnnotationData> {
    let viewer = viewer(harness);
    let id = viewer.selected()?;
    Some(viewer.document().annotations.get(id).unwrap().data.clone())
}

#[test]
fn annotation_mode_creates_selects_and_deletes_with_undo() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let count = viewer(&harness).document().annotations.len();
    assert!(!checked(&mut harness, "Edit", "Edit Annotations"));
    assert!(!enabled(&mut harness, "Edit", "Undo"));
    assert!(!enabled(&mut harness, "Edit", "Delete"));
    press(&mut harness, command(), Key::E);
    assert!(viewer(&harness).editing());
    assert!(checked(&mut harness, "Edit", "Edit Annotations"));
    let p = at_time(&harness, 12.0, 20.0);
    hover(&mut harness, p);
    let status = viewer(&harness).status().left.clone();
    assert!(status.starts_with("Editing annotations · "), "{status}");

    // A drag from right to left creates an annotation of all channels over its range,
    // selected, and doesn't pan.
    let (from, to) = (at_time(&harness, 4.0, 40.0), at_time(&harness, 2.0, 60.0));
    drag(&mut harness, from, to);
    let both = made(&harness);
    assert_eq!(both.len(), 1, "{both:?}");
    let x_tolerance = 1e-4;
    assert!((both[0].0 - 2.0).abs() < x_tolerance, "{both:?}");
    assert!((both[0].1 - 2.0).abs() < x_tolerance, "{both:?}");
    assert_eq!(both[0].2, "BAD");
    assert_eq!(viewer(&harness).document().annotations.len(), count + 1);
    assert_eq!(viewer(&harness).view().time(), 0.0..20.0);
    let id = viewer(&harness)
        .selected()
        .expect("the new annotation is selected");
    let entry = viewer(&harness).legend().entry("BAD").unwrap().clone();
    assert_eq!((entry.count, entry.shown), (1, true));
    assert!(enabled(&mut harness, "Edit", "Undo"));
    assert!(enabled(&mut harness, "Edit", "Delete"));
    // Outlined, and named in the status line.
    let outline = viewer(&harness).outline().unwrap();
    let traces = viewer(&harness).layout().unwrap().traces;
    assert!((outline.left() - to.x).abs() <= 1.0, "{outline:?} {to:?}");
    assert!(
        (outline.right() - from.x).abs() <= 1.0,
        "{outline:?} {from:?}"
    );
    assert_eq!(outline.y_range(), traces.y_range());
    hover(&mut harness, p);
    let status = viewer(&harness).status().left.clone();
    assert!(
        status.starts_with("BAD at 0:02.000 for 2.000 s · "),
        "{status}"
    );
    // With a swatch in its color before it.
    let color = viewer(&harness).legend().entry("BAD").unwrap().color;
    assert_eq!(viewer(&harness).status().swatch, Some(color));

    // A click away from annotations deselects; on it, selects.
    click_at_time(&mut harness, 12.0);
    assert_eq!(viewer(&harness).selected(), None);
    assert_eq!(viewer(&harness).outline(), None);
    assert_eq!(viewer(&harness).status().swatch, None);
    assert!(!enabled(&mut harness, "Edit", "Delete"));
    click_at_time(&mut harness, 3.0);
    assert_eq!(viewer(&harness).selected(), Some(id));

    // ⌫ deletes it; undo brings it back, selected; redo deletes it again.
    press(&mut harness, Modifiers::NONE, Key::Backspace);
    assert!(made(&harness).is_empty());
    assert_eq!(viewer(&harness).selected(), None);
    assert!(
        viewer(&harness)
            .legend()
            .entries()
            .all(|e| e.description != "BAD")
    );
    let shift_command = command() | Modifiers::SHIFT;
    press(&mut harness, command(), Key::Z);
    assert_eq!(made(&harness), both);
    assert_eq!(viewer(&harness).selected(), Some(id));
    press(&mut harness, shift_command, Key::Z);
    assert!(made(&harness).is_empty());
    assert!(!enabled(&mut harness, "Edit", "Redo"));
    assert!(enabled(&mut harness, "Edit", "Undo"));
    // ⌘Y redoes too, Delete deletes too, and the menu items act.
    choose(&mut harness, "Edit", "Undo");
    assert_eq!(made(&harness), both);
    press(&mut harness, Modifiers::NONE, Key::Delete);
    assert!(made(&harness).is_empty());
    choose(&mut harness, "Edit", "Undo");
    click_at_time(&mut harness, 3.0);
    choose(&mut harness, "Edit", "Delete");
    assert!(made(&harness).is_empty());
    press(&mut harness, command(), Key::Z);
    press(&mut harness, command(), Key::Z);
    // The creation undone: nothing left to undo.
    assert!(made(&harness).is_empty());
    assert_eq!(viewer(&harness).selected(), None);
    assert!(!viewer(&harness).history().can_undo());
    assert_eq!(viewer(&harness).document().annotations.len(), count);
    press(&mut harness, command(), Key::Y);
    assert_eq!(made(&harness), both);
    assert_eq!(viewer(&harness).selected(), Some(id));
    // Esc deselects.
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(viewer(&harness).selected(), None);

    // Out of annotation mode, clicks don't select, a drag pans, and undo still works.
    click_at_time(&mut harness, 3.0);
    assert_eq!(viewer(&harness).selected(), Some(id));
    choose(&mut harness, "Edit", "Edit Annotations");
    assert!(!viewer(&harness).editing());
    assert_eq!(viewer(&harness).selected(), None);
    click_at_time(&mut harness, 3.0);
    assert_eq!(viewer(&harness).selected(), None);
    hover(&mut harness, p);
    assert!(
        !viewer(&harness).status().left.contains("Editing"),
        "{}",
        viewer(&harness).status().left
    );
    let (from, to) = (at_time(&harness, 8.0, 40.0), at_time(&harness, 6.0, 40.0));
    drag(&mut harness, from, to);
    assert_eq!(made(&harness), both);
    let time = viewer(&harness).view().time();
    assert!((time.start - 2.0).abs() < 1e-3, "{time:?}");
    press(&mut harness, command(), Key::Z);
    assert!(made(&harness).is_empty());
}

#[test]
fn short_drags_and_an_empty_description_create_nothing() {
    let file = without_triggers("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    // Far enough to be a drag, but less than 3 points sideways.
    let from = at_time(&harness, 12.0, 20.0);
    drag(&mut harness, from, from + egui::vec2(2.5, 30.0));
    assert!(made(&harness).is_empty());
    assert!(!viewer(&harness).history().can_undo());
    // Back to where it started: what it made is gone again.
    let to = from + egui::vec2(40.0, 0.0);
    hover(&mut harness, from);
    harness.drag_at(from);
    harness.step();
    hover(&mut harness, to);
    assert_eq!(made(&harness).len(), 1);
    hover(&mut harness, from + egui::vec2(1.0, 20.0));
    assert!(made(&harness).is_empty());
    harness.drop_at(from + egui::vec2(1.0, 20.0));
    harness.step();
    assert!(made(&harness).is_empty());
    assert!(!viewer(&harness).history().can_undo());
    assert!(
        viewer(&harness)
            .legend()
            .entries()
            .all(|e| e.description != "BAD")
    );

    // Without a description, a drag makes nothing.
    let field = "Description of new annotations";
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    press(&mut harness, command(), Key::A);
    press(&mut harness, Modifiers::NONE, Key::Backspace);
    assert_eq!(viewer(&harness).description(), "");
    drag(&mut harness, from, to);
    assert!(made(&harness).is_empty());
    // A drag past the recording's start stops there.
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    harness.get_by_label(field).type_text("x");
    harness.run_steps(2);
    let start = at_time(&harness, 0.0, 20.0) - egui::vec2(30.0, 0.0);
    let from = at_time(&harness, 1.0, 20.0);
    drag(&mut harness, from, start);
    let got = made(&harness);
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].0, got[0].2.as_str()), (0.0, "x"));
    assert!((got[0].1 - 1.0).abs() < 1e-4, "{got:?}");
    // And past its end, at the end.
    press(&mut harness, Modifiers::NONE, Key::End);
    let from = at_time(&harness, 59.0, 20.0);
    let end = at_time(&harness, 60.0, 20.0) + egui::vec2(30.0, 0.0);
    drag(&mut harness, from, end);
    let got = made(&harness);
    assert_eq!(got.len(), 2);
    assert!((got[1].0 - 59.0).abs() < 1e-4, "{got:?}");
    assert_eq!(got[1].0 + got[1].1, 60.0, "{got:?}");
}

#[test]
fn clicks_select_the_shortest_editable_annotation() {
    // File annotations "trigger 3" from 15.5 s to 16.5 s, events at 10.5 s and 20.5 s, and
    // stim annotations from the Status channel.
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    // Of another description, or the two would merge.
    describe(&mut harness, "long");
    let (a, b) = (at_time(&harness, 12.0, 40.0), at_time(&harness, 19.0, 40.0));
    drag(&mut harness, a, b);
    describe(&mut harness, "BAD");
    // Inside the selected one, a drag would move it.
    press(&mut harness, Modifiers::NONE, Key::Escape);
    let (a, b) = (at_time(&harness, 13.0, 40.0), at_time(&harness, 14.0, 40.0));
    drag(&mut harness, a, b);
    assert_eq!(made(&harness).len(), 2);
    let pick = |harness: &mut Harness<'_, KymoApp>, t: f64| {
        click_at_time(harness, t);
        selected_data(harness).map(|d| (d.description, d.duration.round(), d.origin))
    };
    use kymo_core::Origin::{File, Viewer};
    // The new one is selected, so a click on it selects the next longer one there.
    assert_eq!(selected_data(&harness).unwrap().duration.round(), 1.0);
    assert_eq!(pick(&mut harness, 13.5), Some(("long".into(), 7.0, Viewer)));
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(pick(&mut harness, 13.5), Some(("BAD".into(), 1.0, Viewer)));
    assert_eq!(pick(&mut harness, 12.5), Some(("long".into(), 7.0, Viewer)));
    // A file annotation inside the long one is shorter.
    assert_eq!(
        pick(&mut harness, 16.0),
        Some(("trigger 3".into(), 1.0, File))
    );

    // Stim annotations can't be selected: a click on one alone deselects.
    let stim = viewer(&harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin == kymo_core::Origin::Stim)
        .map(|a| a.data.onset)
        .find(|&t| (1.0..9.0).contains(&t) && (t - 5.5).abs() > 0.5)
        .expect("a stim annotation away from the file's");
    let p = at_time(&harness, stim, 20.0);
    hover(&mut harness, p);
    assert!(
        viewer(&harness).status().left.contains(" · "),
        "{}",
        viewer(&harness).status().left
    );
    assert_eq!(pick(&mut harness, stim), None);

    // A hidden description can't be selected, and hiding it deselects.
    assert_eq!(pick(&mut harness, 13.5).unwrap().0, "BAD");
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show BAD");
    harness.step();
    assert_eq!(viewer(&harness).selected(), None);
    assert_eq!(pick(&mut harness, 13.5).unwrap().0, "long");
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show BAD");
    assert!(pick(&mut harness, 13.5).is_some());
    // Creating one with a hidden description shows them.
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show BAD");
    let (a, b) = (at_time(&harness, 2.0, 40.0), at_time(&harness, 3.0, 40.0));
    drag(&mut harness, a, b);
    assert!(viewer(&harness).legend().entry("BAD").unwrap().shown);
    assert!(pick(&mut harness, 13.5).is_some());

    // An event, "trigger 2", gets an outline around its line.
    assert_eq!(
        pick(&mut harness, 10.5),
        Some(("trigger 2".into(), 0.0, File))
    );
    let outline = viewer(&harness).outline().unwrap();
    let x = at_time(&harness, 10.5, 0.0).x;
    assert!(outline.width() >= 4.0, "{outline:?}");
    assert!(outline.x_range().contains(x), "{outline:?} {x}");
}

/// An annotation lying wholly over shorter ones, as over back-to-back trials, is reached
/// by clicking again: each click on the selected one selects the next longer one there.
#[test]
fn repeated_clicks_reach_every_annotation_under_the_pointer() {
    let file = without_triggers("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    // "a" from 2 to 4.1 s and "b" from 4 to 6 s, back to back; "long" from 3 to 5.5 s,
    // longer than either and nowhere on its own.
    for (description, from, to) in [("a", 2.0, 4.1), ("b", 4.0, 6.0), ("long", 3.0, 5.5)] {
        describe(&mut harness, description);
        press(&mut harness, Modifiers::NONE, Key::Escape);
        let (a, b) = (at_time(&harness, from, 40.0), at_time(&harness, to, 40.0));
        drag(&mut harness, a, b);
    }
    assert_eq!(made(&harness).len(), 3);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    let pick = |harness: &mut Harness<'_, KymoApp>, t: f64| {
        click_at_time(harness, t);
        selected_data(harness).map(|d| d.description)
    };
    assert_eq!(pick(&mut harness, 4.6).as_deref(), Some("b"));
    assert_eq!(pick(&mut harness, 4.6).as_deref(), Some("long"));
    // After the longest, the shortest again.
    assert_eq!(pick(&mut harness, 4.6).as_deref(), Some("b"));
    // Elsewhere, the shortest there first, whatever was selected.
    assert_eq!(pick(&mut harness, 3.5).as_deref(), Some("a"));
    assert_eq!(pick(&mut harness, 3.5).as_deref(), Some("long"));
    // Where three lie, all three in turn.
    assert_eq!(pick(&mut harness, 4.05).as_deref(), Some("b"));
    assert_eq!(pick(&mut harness, 4.05).as_deref(), Some("a"));
    assert_eq!(pick(&mut harness, 4.05).as_deref(), Some("long"));
    assert_eq!(pick(&mut harness, 4.05).as_deref(), Some("b"));
}

#[test]
fn the_description_field_names_new_annotations() {
    // Neither annotations nor stim channels: no legend until annotation mode.
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.annotations.clear();
    spec.channels.retain(|c| !c.trigger);
    let file = TempFile::written(&spec);
    let mut harness = window(file.path());
    ready(&mut harness);
    let field = "Description of new annotations";
    assert!(harness.query_by_label(field).is_none());
    press(&mut harness, command(), Key::E);
    let role = egui::accesskit::Role::CheckBox;
    assert!(
        harness
            .query_by_role_and_label(role, "Annotations")
            .is_none()
    );
    assert_eq!(
        harness.get_by_label(field).value().as_deref(),
        Some(kymo::viewer::DEFAULT_DESCRIPTION)
    );

    // Typed: new annotations take it.
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    press(&mut harness, command(), Key::A);
    harness.get_by_label(field).type_text("spike");
    harness.run_steps(2);
    assert_eq!(viewer(&harness).description(), "spike");
    // ⌘Z and ⇧⌘Z in the field undo and redo the typing (egui undoes quick typing at
    // once); so do the menu's Undo and Redo, which hand the field their keys.
    let shift_command = command() | Modifiers::SHIFT;
    press(&mut harness, command(), Key::Z);
    assert_eq!(viewer(&harness).description(), "BAD");
    press(&mut harness, shift_command, Key::Z);
    assert_eq!(viewer(&harness).description(), "spike");
    harness.event(kymo::app::text_field_key(Action::Undo).unwrap());
    harness.step();
    assert_eq!(viewer(&harness).description(), "BAD");
    harness.event(kymo::app::text_field_key(Action::Redo).unwrap());
    harness.step();
    assert_eq!(viewer(&harness).description(), "spike");
    // Straight from the field to the traces.
    assert!(harness.get_by_label(field).is_focused());
    let (a, b) = (at_time(&harness, 2.0, 40.0), at_time(&harness, 3.0, 40.0));
    drag(&mut harness, a, b);
    let got = made(&harness);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].2, "spike");
    // The drag took the keyboard from the field, which now shows the new annotation's
    // description: ⌫ deletes it, and undo brings it back.
    let selected = "Description of the selected annotation";
    assert!(!harness.get_by_label(selected).is_focused());
    assert_eq!(
        harness.get_by_label(selected).value().as_deref(),
        Some("spike")
    );
    press(&mut harness, Modifiers::NONE, Key::Backspace);
    assert!(made(&harness).is_empty());
    assert_eq!(viewer(&harness).description(), "spike");
    press(&mut harness, command(), Key::Z);
    assert_eq!(made(&harness).len(), 1);
    // Typing new annotations' description doesn't touch the annotations: ⌘Z there leaves
    // them.
    press(&mut harness, Modifiers::NONE, Key::Escape);
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    harness.get_by_label(field).type_text("!");
    harness.run_steps(2);
    press(&mut harness, command(), Key::Z);
    assert_eq!(made(&harness).len(), 1);
    assert_eq!(viewer(&harness).description(), "spike");
    harness.key_press(Key::Enter);
    harness.step();

    // Picked from the menu of descriptions, after typing another.
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    press(&mut harness, command(), Key::A);
    harness.get_by_label(field).type_text("x");
    harness.run_steps(2);
    harness.key_press(Key::Enter);
    harness.step();
    assert_eq!(viewer(&harness).description(), "x");
    harness.get_by_label("Descriptions").click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "spike")
        .click();
    harness.run_steps(2);
    assert_eq!(viewer(&harness).description(), "spike");
    let (a, b) = (at_time(&harness, 7.0, 40.0), at_time(&harness, 8.0, 40.0));
    drag(&mut harness, a, b);
    let got = made(&harness);
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].2, "spike");
    assert_eq!(viewer(&harness).legend().entry("spike").unwrap().count, 2);

    // Out of annotation mode the field goes, the legend stays.
    press(&mut harness, command(), Key::E);
    assert!(harness.query_by_label(field).is_none());
    harness.get_by_label("Show spike");
}

/// The wgpu device on Metal, Vulkan or DX12 (traces need storage buffers in vertex shaders,
/// which OpenGL lacks), created once and shared by every window rendered on a GPU, as
/// `render`'s tests share theirs; `None` without such an adapter.
///
/// One instance and device per window, created and dropped by several tests at once,
/// crashed lavapipe (Mesa's software Vulkan driver, which CI uses on Linux) now and then
/// with a segfault that took the whole test binary down.
fn shared_gpu() -> Option<&'static egui_wgpu::WgpuSetupExisting> {
    use eframe::egui_wgpu::wgpu;
    static GPU: OnceLock<Option<egui_wgpu::WgpuSetupExisting>> = OnceLock::new();
    GPU.get_or_init(|| {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        desc.backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(desc);
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        // The device eframe would ask for.
        let device_descriptor =
            egui_wgpu::WgpuSetupCreateNew::without_display_handle().device_descriptor;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&device_descriptor(&adapter))).ok()?;
        Some(egui_wgpu::WgpuSetupExisting {
            instance,
            adapter,
            device,
            queue,
        })
    })
    .as_ref()
}

/// kittest's wgpu setup on the shared GPU (see [`shared_gpu`]), or `None` without one: the
/// test then passes with a message, unless `KYMO_REQUIRE_GPU` is set.
fn gpu_setup() -> Option<egui_wgpu::WgpuSetup> {
    let Some(gpu) = shared_gpu() else {
        assert!(
            std::env::var_os("KYMO_REQUIRE_GPU").is_none_or(|v| v.is_empty()),
            "no GPU adapter, and KYMO_REQUIRE_GPU is set"
        );
        eprintln!("skipped: no GPU adapter");
        return None;
    };
    Some(egui_wgpu::WgpuSetup::Existing(gpu.clone()))
}

/// The window opening `path` on a GPU, at one pixel per point.
fn gpu_window(path: &Path, setup: egui_wgpu::WgpuSetup) -> Harness<'static, KymoApp> {
    let path = path.to_owned();
    Harness::builder()
        .with_size(SIZE)
        .with_context_setup(system_fonts)
        .wgpu_setup(setup)
        .build_eframe(move |cc| {
            let gpu = Gpu::new(cc.wgpu_render_state.as_ref());
            assert_eq!(gpu.error(), None);
            KymoApp::new(&cc.egui_ctx, gpu, options(), Some(path))
        })
}

/// Turns anti-aliasing off, so every trace pixel has its color exactly (anti-aliased edges
/// blend with the background and the annotations behind).
fn whole_pixels(harness: &mut Harness<'_, KymoApp>) {
    let settings = *harness.state().settings();
    harness.state_mut().set_settings(Settings {
        antialias: false,
        ..settings
    });
    harness.run_steps(2);
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
fn anti_aliased_traces_blend_their_edges() {
    let Some(setup) = gpu_setup() else { return };
    let file = TempFile::preset("bdf+c-small");
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    // Distinct colors in the trace area: whole pixels give the background, the grid and
    // the traces' colors; anti-aliased edges give many shades between them.
    let colors = |harness: &mut Harness<'_, KymoApp>| {
        traces_image(harness)
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
    };
    let off = colors(&mut harness);
    let ctx = harness.ctx.clone();
    harness.state_mut().perform(&ctx, Action::ToggleAntialias);
    harness.run_steps(2);
    assert!(harness.state().settings().antialias);
    let on = colors(&mut harness);
    assert!(
        on >= off + 20,
        "{on} colors with anti-aliasing, {off} without"
    );
}

#[test]
fn opening_another_recording_never_draws_the_last_ones_tiles() {
    let Some(setup) = gpu_setup() else { return };
    // The same layout, so the same tile keys, with different values.
    let first = seeded("bdf+c-small", 1);
    let second = seeded("bdf+c-small", 2);
    // The traces alone: the annotations' labels come from egui's font atlas, which holds
    // whatever text the window showed before (status lines while loading), so their
    // pixels may differ a little between windows.
    let traces = |harness: &mut Harness<'_, KymoApp>| {
        click_in_list(harness, egui::accesskit::Role::CheckBox, "Annotations");
        traces_image(harness)
    };

    let mut fresh = gpu_window(second.path(), setup.clone());
    ready(&mut fresh);
    let want = traces(&mut fresh);

    let mut switched = gpu_window(first.path(), setup);
    ready(&mut switched);
    let before = traces(&mut switched);
    assert_ne!(before, want, "the two recordings look the same");
    let ctx = switched.ctx.clone();
    switched.state_mut().open(&ctx, second.path().to_owned());
    ready(&mut switched);
    let after = traces(&mut switched);
    let width = viewer(&switched).layout().unwrap().traces.width() as usize;
    assert!(
        after == want,
        "tiles of the first recording drawn: {}",
        differences(&after, &want, width)
    );
}

/// Where two images of `width` pixels per row differ, for messages.
fn differences(a: &[[u8; 4]], b: &[[u8; 4]], width: usize) -> String {
    let mut count = 0;
    let mut most = 0;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (i, (p, q)) in a.iter().zip(b).enumerate() {
        if p != q {
            count += 1;
            most = most.max((0..4).map(|c| p[c].abs_diff(q[c])).max().unwrap_or(0));
            let (x, y) = (i % width, i / width);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    format!(
        "{count} of {} pixels differ, by up to {most}, in x {x0}..={x1}, y {y0}..={y1}",
        a.len()
    )
}

#[test]
fn dc_removal_brings_an_offset_trace_into_its_row() {
    let Some(setup) = gpu_setup() else { return };
    let file = offset_preset("bdf+c-small", -21_000.0);
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    // Without the annotations' labels, some of which are near the trace color.
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Annotations");
    let trace = harness.ctx.global_style().visuals.text_color();
    // Pixels of the first row (A1) in the trace color.
    let a1_pixels = |harness: &mut Harness<'_, KymoApp>| {
        let traces = viewer(harness).layout().unwrap().traces;
        let rows = viewer(harness).view().rows();
        let width = traces.width() as usize;
        let row_height = traces.height() as usize / rows;
        traces_image(harness)[..width * row_height]
            .iter()
            .filter(|p| {
                let d = |a: u8, b: u8| i32::from(a).abs_diff(i32::from(b));
                d(p[0], trace.r()) + d(p[1], trace.g()) + d(p[2], trace.b()) < 60
            })
            .count()
    };
    // 21 mV below its row's middle, far outside it (a spike may cross it).
    let off = a1_pixels(&mut harness);
    harness.key_press_modifiers(command(), Key::D);
    harness.step();
    step_until(&mut harness, "the means", baselines_are_window_means);
    let on = a1_pixels(&mut harness);
    assert!(
        on > 500 && on > 20 * off,
        "{off} pixels without DC removal, {on} with"
    );
}

#[test]
fn the_gpu_budget_follows_the_settings() {
    let Some(setup) = gpu_setup() else { return };
    let file = TempFile::preset("bdf+c-small");
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    let gpu = |h: &Harness<'_, KymoApp>| h.state().gpu().budget();
    assert_eq!(gpu(&harness), Some(256 << 20));
    harness.key_press_modifiers(command(), Key::Comma);
    harness.step();
    set_field(&mut harness, "GPU memory", 96.0);
    assert_eq!(gpu(&harness), Some(96 << 20));
    // The traces stay drawn.
    assert!(viewer(&harness).complete());
}

#[test]
fn traces_take_their_types_colors() {
    let Some(setup) = gpu_setup() else { return };
    // TRIGGER, a flat EEG signal, is stim too, but without a trigger mask.
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.channels[1].label = "EOG L".into();
    spec.channels[2].label = "ECG".into();
    spec.channels[3].label = "TRIGGER".into();
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    whole_pixels(&mut harness);
    let visuals = harness.ctx.global_style().visuals.clone();
    // The rows, top to bottom; the last is Status.
    let kinds = [
        kymo_core::ChannelKind::Eeg,
        kymo_core::ChannelKind::Eog,
        kymo_core::ChannelKind::Ecg,
        kymo_core::ChannelKind::Stim,
        kymo_core::ChannelKind::Stim,
    ];
    let colors: Vec<_> = kinds.iter().map(|&k| trace_color(k, &visuals)).collect();
    // Per row, the pixels in each type's color.
    let counts = |harness: &mut Harness<'_, KymoApp>| {
        let traces = viewer(harness).layout().unwrap().traces;
        let width = traces.width() as usize;
        let row_height = traces.height() as usize / kinds.len();
        let image = traces_image(harness);
        (0..kinds.len())
            .map(|row| {
                let pixels = &image[row * row_height * width..(row + 1) * row_height * width];
                colors
                    .iter()
                    .map(|c| pixels.iter().filter(|p| near(p, *c)).count())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let before = counts(&mut harness);
    for (row, kind) in kinds.iter().enumerate() {
        let own = before[row][row];
        assert!(own > 200, "row {row} ({kind:?}): {:?}", before[row]);
        for (other, count) in before[row].iter().enumerate() {
            if colors[other] != colors[row] {
                assert!(*count < own / 20, "row {row} ({kind:?}): {:?}", before[row]);
            }
        }
    }

    // A bad channel is grayed out, whatever its type.
    harness.get_by_label("ECG bad").click();
    harness.run_steps(2);
    let after = counts(&mut harness);
    assert!(after[2][2] < before[2][2] / 20, "{:?}", after[2]);
}

/// The title bar's themes the last frame asked for.
fn title_bar_themes(harness: &Harness<'_, KymoApp>) -> Vec<egui::SystemTheme> {
    harness
        .output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|viewport| {
            viewport
                .commands
                .iter()
                .filter_map(|c| match c {
                    egui::ViewportCommand::SetTheme(theme) => Some(*theme),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_theme_follows_the_setting_and_the_system() {
    use egui::accesskit::Role;
    use egui::{SystemTheme, Theme as Look};
    let file = without_triggers("edf+d-mixed");
    let mut harness = screenshot_window(file.path());
    harness.input_mut().system_theme = Some(Look::Light);
    ready(&mut harness);
    // What the window shows: egui's theme, the trace area's background (its most common
    // color) and a legend swatch, against the colors of the theme egui uses now.
    let shown = |harness: &mut Harness<'_, KymoApp>| {
        let image = harness.render().unwrap();
        let pixel = |x: f32, y: f32| {
            egui::Color32::from_rgba_premultiplied(
                image.get_pixel(x as u32, y as u32)[0],
                image.get_pixel(x as u32, y as u32)[1],
                image.get_pixel(x as u32, y as u32)[2],
                255,
            )
        };
        let traces = viewer(harness).layout().unwrap().traces;
        let mut counts = std::collections::HashMap::new();
        for y in (traces.top() as u32 + 1..traces.bottom() as u32 - 1).step_by(3) {
            for x in (traces.left() as u32 + 1..traces.right() as u32 - 1).step_by(3) {
                *counts.entry(pixel(x as f32, y as f32)).or_insert(0) += 1;
            }
        }
        let background = counts.into_iter().max_by_key(|&(_, n)| n).unwrap().0;
        // A legend row clear of the Settings window and its shadow (below it), and of the
        // fade at the legend's bottom edge.
        let settings_bottom = harness.query_by_label("Close window").map_or(0.0, |_| {
            harness.get_by_label("Restore Defaults").rect().bottom() + 40.0
        });
        let descriptions: Vec<String> = viewer(harness)
            .legend()
            .entries()
            .map(|e| e.description.clone())
            .collect();
        let (description, swatch) = descriptions
            .iter()
            .filter_map(|d| {
                let rect = harness.query_by_label(&format!("Color of {d}"))?.rect();
                (rect.top() > settings_bottom && rect.bottom() < SIZE.y - 60.0)
                    .then(|| (d.clone(), rect.center()))
            })
            .next()
            .expect("a legend swatch below the Settings window");
        let color = viewer(harness).legend().entry(&description).unwrap().color;
        let visuals = harness.ctx.global_style().visuals.clone();
        assert_eq!(background, visuals.extreme_bg_color, "trace area");
        // Within rounding: the swatch's variants differ by far more.
        let swatch = pixel(swatch.x, swatch.y);
        let want = annotation_color(color, &visuals);
        assert!(
            near(&swatch.to_array(), want),
            "swatch {swatch:?}, not {want:?}"
        );
        assert_eq!(visuals.dark_mode, harness.ctx.theme() == Look::Dark);
        harness.ctx.theme()
    };
    // Kymo's own colors differ between the themes.
    let [light, dark] = [egui::Visuals::light(), egui::Visuals::dark()];
    assert_ne!(light.extreme_bg_color, dark.extreme_bg_color);
    let palette0 = AnnotationColor::Palette(0);
    assert_ne!(
        annotation_color(palette0, &light),
        annotation_color(palette0, &dark)
    );

    // System: the system's theme as it changes, the title bar left to the system.
    assert_eq!(harness.state().settings().theme, Theme::System);
    assert_eq!(shown(&mut harness), Look::Light);
    harness.input_mut().system_theme = Some(Look::Dark);
    harness.step();
    assert_eq!(title_bar_themes(&harness), [SystemTheme::SystemDefault]);
    harness.run_steps(2);
    assert_eq!(shown(&mut harness), Look::Dark);

    // Light or Dark from Settings, whatever the system's theme; the title bar is set again
    // when the system's changes, as Windows puts it back to the system's mode then.
    harness.key_press_modifiers(command(), Key::Comma);
    harness.run_steps(2);
    for (theme, look, title_bar) in [
        (Theme::Light, Look::Light, SystemTheme::Light),
        (Theme::Dark, Look::Dark, SystemTheme::Dark),
    ] {
        harness
            .get_by_role_and_label(Role::RadioButton, theme.label())
            .click();
        harness.step();
        assert_eq!(harness.state().settings().theme, theme);
        assert_eq!(title_bar_themes(&harness), [title_bar], "{theme:?}");
        harness.run_steps(2);
        assert_eq!(title_bar_themes(&harness), [], "{theme:?} again");
        assert_eq!(shown(&mut harness), look, "{theme:?}");
        for system in [Look::Light, Look::Dark] {
            harness.input_mut().system_theme = Some(system);
            harness.step();
            harness.step();
            assert_eq!(
                shown(&mut harness),
                look,
                "{theme:?} on a {system:?} system"
            );
        }
        harness.input_mut().system_theme = Some(Look::Light);
        harness.step();
        assert_eq!(
            title_bar_themes(&harness),
            [title_bar],
            "{theme:?}, system changed"
        );
        harness.run_steps(2);
    }

    // Kept for the next run, which starts in it.
    let mut storage = Memory::default();
    eframe::App::save(harness.state_mut(), &mut storage);
    let settings = Settings::load(Some(&storage));
    assert_eq!(settings.theme, Theme::Dark);
    let path = file.path().to_owned();
    let mut next = Harness::builder().with_size(SIZE).build_eframe(move |cc| {
        KymoApp::new(
            &cc.egui_ctx,
            Gpu::none("no GPU in this test"),
            Options {
                settings,
                ..options()
            },
            Some(path),
        )
    });
    next.input_mut().system_theme = Some(Look::Light);
    next.step();
    assert_eq!(next.ctx.theme(), Look::Dark);
    assert_eq!(title_bar_themes(&next), [SystemTheme::Dark]);

    // Back to System: the system's theme again, and the title bar the system's.
    harness
        .get_by_role_and_label(Role::RadioButton, "System")
        .click();
    harness.step();
    assert_eq!(title_bar_themes(&harness), [SystemTheme::SystemDefault]);
    harness.run_steps(2);
    assert_eq!(shown(&mut harness), Look::Light);
}

#[test]
fn system_follows_the_app_mode_where_it_is_read() {
    // Windows: System follows the default app mode as Kymo reads it, not the system's
    // theme as winit reports it (which a forced theme skews there).
    use egui::{SystemTheme, Theme as Look};
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    let mode = AppMode::new();
    mode.set(Some(false));
    harness.state_mut().follow_app_mode(mode.clone());
    harness.input_mut().system_theme = Some(Look::Dark);
    ready(&mut harness);
    assert_eq!(harness.ctx.theme(), Look::Light);

    // The app mode changes: all of Kymo follows, the title bar set as such.
    mode.set(Some(true));
    harness.step();
    assert_eq!(harness.ctx.theme(), Look::Dark);
    assert_eq!(title_bar_themes(&harness), [SystemTheme::Dark]);
    // winit's report changes alone: the app mode still rules, the title bar set again.
    harness.input_mut().system_theme = Some(Look::Light);
    harness.step();
    assert_eq!(harness.ctx.theme(), Look::Dark);
    assert_eq!(title_bar_themes(&harness), [SystemTheme::Dark]);

    // Light, whatever the app mode.
    let set = |harness: &mut Harness<'_, KymoApp>, theme| {
        let settings = *harness.state().settings();
        harness
            .state_mut()
            .set_settings(Settings { theme, ..settings });
        harness.step();
    };
    set(&mut harness, Theme::Light);
    assert_eq!(harness.ctx.theme(), Look::Light);
    assert_eq!(title_bar_themes(&harness), [SystemTheme::Light]);
    mode.set(Some(false));
    harness.step();
    mode.set(Some(true));
    harness.step();
    assert_eq!(harness.ctx.theme(), Look::Light);

    // Back to System: the app mode's, whatever winit reported meanwhile.
    set(&mut harness, Theme::System);
    assert_eq!(harness.ctx.theme(), Look::Dark);
    assert_eq!(title_bar_themes(&harness), [SystemTheme::Dark]);

    // An app mode that can't be read: the system's theme as winit reports it.
    mode.set(None);
    harness.step();
    assert_eq!(harness.ctx.theme(), Look::Light);
    assert_eq!(title_bar_themes(&harness), [SystemTheme::SystemDefault]);
}

#[test]
fn the_theme_is_in_force_before_the_first_frame() {
    // The startup file dialog opens while the second frame is built and leaves the first
    // on screen, so that has to be in the theme already (egui's default is dark).
    use egui::Theme as Look;
    let app = |theme| {
        let options = Options {
            settings: Settings {
                theme,
                ..Settings::default()
            },
            ..options()
        };
        let ctx = egui::Context::default();
        let app = KymoApp::new(&ctx, Gpu::none("no GPU in this test"), options, None);
        (ctx, app)
    };
    let (ctx, mut forced) = app(Theme::Light);
    assert_eq!(ctx.theme(), Look::Dark);
    forced.start(&ctx);
    assert_eq!(ctx.theme(), Look::Light);

    // System with an app mode that is read (Linux, Windows): its mode, light here.
    let (ctx, mut system) = app(Theme::System);
    let mode = AppMode::new();
    mode.set(Some(false));
    system.follow_app_mode(mode);
    system.start(&ctx);
    assert_eq!(ctx.theme(), Look::Light);
}

#[test]
fn traces_take_the_themes_colors() {
    let Some(setup) = gpu_setup() else { return };
    // Rows A1, A2 (EEG), "EOG L", ECG, Status.
    let file = typed_preset();
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    whole_pixels(&mut harness);
    let [light, dark] = [egui::Visuals::light(), egui::Visuals::dark()];
    // Pixels of the EOG row near `color`.
    let count = |harness: &mut Harness<'_, KymoApp>, color: egui::Color32| {
        let traces = viewer(harness).layout().unwrap().traces;
        let width = traces.width() as usize;
        let row_height = traces.height() as usize / 5;
        traces_image(harness)[2 * row_height * width..3 * row_height * width]
            .iter()
            .filter(|p| near(p, color))
            .count()
    };
    for (theme, own, other) in [(Theme::Light, &light, &dark), (Theme::Dark, &dark, &light)] {
        let settings = *harness.state().settings();
        harness
            .state_mut()
            .set_settings(Settings { theme, ..settings });
        harness.run_steps(2);
        let ours = count(&mut harness, trace_color(ChannelKind::Eog, own));
        let theirs = count(&mut harness, trace_color(ChannelKind::Eog, other));
        assert!(
            ours > 200 && theirs < ours / 20,
            "{theme:?}: {ours} and {theirs}"
        );
    }
}

#[test]
fn traces_take_picked_type_colors() {
    let Some(setup) = gpu_setup() else { return };
    // Rows A1, A2 (EEG), "EOG L", ECG, Status.
    let file = typed_preset();
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    whole_pixels(&mut harness);
    let green = egui::Color32::from_rgb(10, 200, 30);
    let visuals = harness.ctx.global_style().visuals.clone();
    let blue = trace_color(ChannelKind::Eog, &visuals);
    // Pixels of the EOG row near `color`.
    let count = |harness: &mut Harness<'_, KymoApp>, color: egui::Color32| {
        let traces = viewer(harness).layout().unwrap().traces;
        let width = traces.width() as usize;
        let row_height = traces.height() as usize / 5;
        let image = traces_image(harness);
        image[2 * row_height * width..3 * row_height * width]
            .iter()
            .filter(|p| near(p, color))
            .count()
    };
    // Green pixels of the sidebar's EOG type row and channel name.
    let sidebar = |harness: &mut Harness<'_, KymoApp>| {
        let rects = [
            harness
                .get_by_role_and_label(egui::accesskit::Role::CheckBox, "EOG")
                .rect(),
            harness.get_by_label("EOG L").rect(),
        ];
        let image = harness.render().unwrap();
        rects.map(|r| {
            (r.left() as u32..r.right() as u32)
                .flat_map(|x| (r.top() as u32..r.bottom() as u32).map(move |y| (x, y)))
                .filter(|&(x, y)| near(&image.get_pixel(x, y).0, green))
                .count()
        })
    };
    let before = (count(&mut harness, blue), count(&mut harness, green));
    assert!(before.0 > 200 && before.1 == 0, "{before:?}");
    assert_eq!(sidebar(&mut harness), [0, 0]);

    harness.key_press_modifiers(command(), Key::Comma);
    harness.step();
    pick(&mut harness, "Color of EOG", [10, 200, 30]);
    harness.key_press(Key::Escape);
    harness.get_by_label("Close window").click();
    harness.run_steps(3);
    let after = (count(&mut harness, blue), count(&mut harness, green));
    assert!(
        after.0 < before.0 / 20 && after.1 > before.0 / 2,
        "{after:?}"
    );
    let named = sidebar(&mut harness);
    assert!(named.iter().all(|&n| n > 5), "{named:?}");

    // A bad channel stays gray.
    harness.get_by_label("EOG L bad").click();
    harness.run_steps(2);
    let bad = count(&mut harness, green);
    assert!(bad < after.1 / 20, "{bad}");
}

#[test]
fn stim_traces_stay_in_their_row() {
    let Some(setup) = gpu_setup() else { return };
    // Status, the last row, holds codes up to 255 under status bits around 2^20; at a stim
    // scale of 1 per row, either would reach far beyond the row.
    let file = TempFile::preset("bdf+c-small");
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    whole_pixels(&mut harness);
    let visuals = harness.ctx.global_style().visuals.clone();
    let gray = trace_color(kymo_core::ChannelKind::Stim, &visuals);
    let text = visuals.text_color();
    // Per row, the pixels in the stim color and in the text color; and the pixel line of
    // the last row with the most stim-colored pixels, from the row's top.
    let counts = |harness: &mut Harness<'_, KymoApp>| {
        let traces = viewer(harness).layout().unwrap().traces;
        let rows = viewer(harness).view().rows();
        assert_eq!(rows, 5);
        let width = traces.width() as usize;
        let row_height = traces.height() as usize / rows;
        let image = traces_image(harness);
        let counts: Vec<(usize, usize)> = (0..rows)
            .map(|row| {
                let pixels = &image[row * row_height * width..(row + 1) * row_height * width];
                let count = |c| pixels.iter().filter(|p| near(p, c)).count();
                (count(gray), count(text))
            })
            .collect();
        let lines = image[4 * row_height * width..5 * row_height * width].chunks_exact(width);
        let busiest = lines
            .map(|line| line.iter().filter(|p| near(p, gray)).count())
            .enumerate()
            .max_by_key(|&(_, n)| n)
            .unwrap()
            .0;
        (counts, busiest as f32 / row_height as f32)
    };
    let check = |(counts, busiest): &(Vec<(usize, usize)>, f32)| {
        let (status, _) = counts[4];
        assert!(status > 500, "{counts:?}");
        for &(gray, text) in &counts[..4] {
            assert!(gray < status / 50, "{counts:?}");
            assert!(text > 200, "{counts:?}");
        }
        // Mostly code 0, in the middle of the row.
        assert!(
            (busiest - 0.5).abs() < 0.05,
            "busiest line at {busiest} of the row"
        );
    };
    check(&counts(&mut harness));
    harness.key_press_modifiers(command(), Key::D);
    harness.step();
    step_until(&mut harness, "the means", baselines_are_window_means);
    check(&counts(&mut harness));
}

/// Bands tint the background behind the traces, which keep their color; event lines are
/// solid.
#[test]
fn annotations_lie_behind_the_traces() {
    let Some(setup) = gpu_setup() else { return };
    // In the first 20 s: "trigger 0" at 0.5 s (color 0), "trigger 1" from 5.5 s to 6.5 s
    // (color 1), and so on.
    let file = TempFile::preset("bdf+c-small");
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    let visuals = harness.ctx.global_style().visuals.clone();
    let marks = viewer(&harness).marks().clone();
    let traces = viewer(&harness).layout().unwrap().traces;
    let (width, height) = (traces.width() as usize, traces.height() as usize);
    let image = traces_image(&mut harness);
    let column =
        |c: u32| -> Vec<[u8; 4]> { (0..height).map(|y| image[y * width + c as usize]).collect() };
    let mode = |pixels: &[[u8; 4]]| -> [u8; 4] {
        let mut counts = std::collections::HashMap::new();
        for p in pixels {
            *counts.entry(*p).or_insert(0) += 1;
        }
        counts.into_iter().max_by_key(|&(_, n)| n).unwrap().0
    };
    let distance = |p: [u8; 4], c: egui::Color32| {
        let d = |a: u8, b: u8| i32::from(a).abs_diff(i32::from(b));
        d(p[0], c.r()) + d(p[1], c.g()) + d(p[2], c.b())
    };

    let band = marks
        .bands
        .iter()
        .find(|b| b.color == AnnotationColor::Palette(1))
        .unwrap()
        .clone();
    assert!(band.columns.len() > 10, "{band:?}");
    let marked = |c: u32| {
        marks
            .bands
            .iter()
            .chain(&marks.lines)
            .any(|m| m.columns.start <= c + 2 && c < m.columns.end + 2)
    };
    let plain = (0..width as u32).find(|&c| !marked(c)).unwrap();
    let background = mode(&column(plain));
    let tinted = mode(&column((band.columns.start + band.columns.end) / 2));
    let band_color = annotation_color(AnnotationColor::Palette(1), &visuals);
    assert!(
        tinted != background && distance(tinted, band_color) < distance(background, band_color),
        "band {tinted:?}, background {background:?}"
    );
    // The traces cross the band in their own color.
    let trace = trace_color(kymo_core::ChannelKind::Eeg, &visuals);
    let on_top = band
        .columns
        .clone()
        .flat_map(column)
        .filter(|&p| distance(p, trace) < 12)
        .count();
    assert!(on_top > 20, "{on_top} trace pixels in the band");

    let line = marks
        .lines
        .iter()
        .find(|l| l.color == AnnotationColor::Palette(0))
        .unwrap();
    let solid = mode(&column(line.columns.start));
    assert!(
        distance(
            solid,
            annotation_color(AnnotationColor::Palette(0), &visuals)
        ) < 12,
        "line {solid:?}"
    );
}

/// Whether a pixel is within a small distance of `color`./// Whether a pixel is within a small distance of `color`.
fn near(pixel: &[u8; 4], color: egui::Color32) -> bool {
    let d = |a: u8, b: u8| i32::from(a).abs_diff(i32::from(b));
    d(pixel[0], color.r()) + d(pixel[1], color.g()) + d(pixel[2], color.b()) < 40
}

/// Renders the window on a GPU and writes it to `KYMO_DUMP` (a directory) if set, showing
/// `KYMO_UI_FILE` or a generated recording.
#[test]
fn renders_the_window() {
    if std::env::var_os("KYMO_DUMP").is_none() {
        return;
    }
    let generated = typed_preset();
    let path = std::env::var_os("KYMO_UI_FILE").map_or(generated.path().to_owned(), PathBuf::from);
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .with_pixels_per_point(2.0)
        .with_context_setup(system_fonts)
        .wgpu()
        .build_eframe(|cc| {
            KymoApp::new(
                &cc.egui_ctx,
                Gpu::new(cc.wgpu_render_state.as_ref()),
                options(),
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

/// The selected annotation's onset and end.
fn selected_span(harness: &Harness<'_, KymoApp>) -> (f64, f64) {
    let d = selected_data(harness).expect("a selection");
    (d.onset, d.end())
}

#[track_caller]
fn assert_span(got: (f64, f64), want: (f64, f64)) {
    assert!(
        (got.0 - want.0).abs() < 1e-3 && (got.1 - want.1).abs() < 1e-3,
        "{got:?} != {want:?}"
    );
}

fn cursor(harness: &Harness<'_, KymoApp>) -> egui::CursorIcon {
    harness.output().platform_output.cursor_icon
}

#[test]
fn dragging_the_selected_annotation_moves_and_resizes_it() {
    let file = without_triggers("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    let at = |harness: &Harness<'_, KymoApp>, t| at_time(harness, t, 40.0);
    let (a, b) = (at(&harness, 2.0), at(&harness, 6.0));
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (2.0, 6.0));
    // Inside, it moves; at an edge, or just outside, the edge moves.
    let (a, b) = (at(&harness, 4.0), at(&harness, 5.0));
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (3.0, 7.0));
    let (a, b) = (at(&harness, 7.0), at(&harness, 8.0));
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (3.0, 8.0));
    // The edge keeps its distance to the pointer.
    let outside = egui::vec2(3.0, 0.0);
    let (a, b) = (at(&harness, 3.0) - outside, at(&harness, 2.5) - outside);
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (2.5, 8.0));
    // Past the other edge, they swap.
    let (a, b) = (at(&harness, 8.0), at(&harness, 1.0));
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (1.0, 2.5));
    assert_eq!(made(&harness).len(), 1);
    // Each drag is one step.
    for want in [(2.5, 8.0), (3.0, 8.0), (3.0, 7.0), (2.0, 6.0)] {
        press(&mut harness, command(), Key::Z);
        assert_span(selected_span(&harness), want);
    }
    press(&mut harness, Modifiers::SHIFT | command(), Key::Z);
    assert_span(selected_span(&harness), (3.0, 7.0));
    // Moving stops at the start, keeping the duration.
    let (a, b) = (at(&harness, 5.0), at(&harness, 0.0) - egui::vec2(60.0, 0.0));
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (0.0, 4.0));
    // So does an edge grabbed from inside, which keeps its distance to the pointer.
    let inside = egui::vec2(3.0, 0.0);
    let (a, b) = (
        at(&harness, 0.0) + inside,
        at(&harness, 0.0) - egui::vec2(60.0, 0.0),
    );
    drag(&mut harness, a, b);
    assert_eq!(selected_span(&harness).0, 0.0);
    // And at the end.
    press(&mut harness, Modifiers::NONE, Key::End);
    let (a, b) = (at(&harness, 50.0), at(&harness, 55.0));
    drag(&mut harness, a, b);
    let (a, b) = (
        at(&harness, 52.0),
        at(&harness, 60.0) + egui::vec2(60.0, 0.0),
    );
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (55.0, 60.0));
    // A resize stops there too.
    let (a, b) = (
        at(&harness, 55.0),
        at(&harness, 60.0) + egui::vec2(60.0, 0.0),
    );
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (60.0, 60.0));
    press(&mut harness, command(), Key::Z);
    assert_eq!(made(&harness).len(), 2);

    // The pointer shows what a drag would do.
    for (t, icon) in [
        (57.5, egui::CursorIcon::Grab),
        (55.0, egui::CursorIcon::ResizeHorizontal),
        (60.0, egui::CursorIcon::ResizeHorizontal),
        (45.0, egui::CursorIcon::Crosshair),
    ] {
        let p = at(&harness, t) - egui::vec2(if t == 60.0 { 1.0 } else { 0.0 }, 0.0);
        hover(&mut harness, p);
        harness.step();
        assert_eq!(cursor(&harness), icon, "{t}");
    }
    let p = at(&harness, 57.5);
    harness.drag_at(p);
    harness.step();
    hover(&mut harness, p + egui::vec2(20.0, 0.0));
    assert_eq!(cursor(&harness), egui::CursorIcon::Grabbing);
    harness.drop_at(p + egui::vec2(20.0, 0.0));
    harness.step();
}

#[test]
fn a_drag_taken_back_or_cancelled_changes_nothing() {
    let file = without_triggers("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    let at = |harness: &Harness<'_, KymoApp>, t| at_time(harness, t, 40.0);
    let (a, b) = (at(&harness, 2.0), at(&harness, 6.0));
    drag(&mut harness, a, b);
    // Out and back: it moves while dragged, and ends where it was, with no step.
    let p = at(&harness, 4.0);
    hover(&mut harness, p);
    harness.drag_at(p);
    harness.step();
    let q = at(&harness, 5.0);
    hover(&mut harness, q);
    assert_span(selected_span(&harness), (3.0, 7.0));
    hover(&mut harness, p + egui::vec2(1.0, 0.0));
    harness.drop_at(p + egui::vec2(1.0, 0.0));
    harness.step();
    assert_span(selected_span(&harness), (2.0, 6.0));
    // Undo takes back the create, so the drag recorded nothing.
    press(&mut harness, command(), Key::Z);
    assert!(made(&harness).is_empty());
    press(&mut harness, Modifiers::SHIFT | command(), Key::Z);

    // Esc puts it back and ends the drag, keeping the selection; again, it deselects.
    harness.drag_at(p);
    harness.step();
    let q = at(&harness, 5.0);
    hover(&mut harness, q);
    // Undo and Delete wait for the drag to end.
    press(&mut harness, command(), Key::Z);
    press(&mut harness, Modifiers::NONE, Key::Backspace);
    assert_span(selected_span(&harness), (3.0, 7.0));
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_span(selected_span(&harness), (2.0, 6.0));
    let q = at(&harness, 6.0);
    hover(&mut harness, q);
    harness.drop_at(at(&harness, 6.0));
    harness.step();
    assert_span(selected_span(&harness), (2.0, 6.0));
    press(&mut harness, command(), Key::Z);
    assert!(made(&harness).is_empty());
    press(&mut harness, Modifiers::SHIFT | command(), Key::Z);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(viewer(&harness).selected(), None);

    // Leaving annotation mode during a drag keeps it, as one step.
    click_at_time(&mut harness, 4.0);
    let p = at(&harness, 4.0);
    hover(&mut harness, p);
    harness.drag_at(p);
    harness.step();
    let q = at(&harness, 5.0);
    hover(&mut harness, q);
    press(&mut harness, command(), Key::E);
    harness.drop_at(q);
    harness.step();
    assert_span(
        (
            made(&harness)[0].0,
            made(&harness)[0].0 + made(&harness)[0].1,
        ),
        (3.0, 7.0),
    );
    press(&mut harness, command(), Key::Z);
    assert_span(
        (
            made(&harness)[0].0,
            made(&harness)[0].0 + made(&harness)[0].1,
        ),
        (2.0, 6.0),
    );
    press(&mut harness, command(), Key::E);

    // Esc takes back a create as well.
    let p = at(&harness, 10.0);
    hover(&mut harness, p);
    harness.drag_at(p);
    harness.step();
    let q = at(&harness, 12.0);
    hover(&mut harness, q);
    assert_eq!(made(&harness).len(), 2);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(made(&harness).len(), 1);
    let q = at(&harness, 13.0);
    hover(&mut harness, q);
    harness.drop_at(at(&harness, 13.0));
    harness.step();
    assert_eq!(made(&harness).len(), 1);

    // An annotation that isn't selected isn't grabbed: a drag over it creates one.
    describe(&mut harness, "x");
    let (a, b) = (at(&harness, 3.0), at(&harness, 4.0));
    drag(&mut harness, a, b);
    let got = made(&harness);
    assert_eq!(got.len(), 2);
    assert_span((got[0].0, got[0].0 + got[0].1), (2.0, 6.0));
    assert_eq!(got[1].2, "x");
}

/// The viewer's and the file's annotations of `description`, as onset and end, by onset.
fn spans(harness: &Harness<'_, KymoApp>, description: &str) -> Vec<(f64, f64, kymo_core::Origin)> {
    viewer(harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.description == description)
        .map(|a| (a.data.onset, a.data.end(), a.data.origin))
        .collect()
}

#[track_caller]
fn assert_spans(got: Vec<(f64, f64, kymo_core::Origin)>, want: &[(f64, f64)]) {
    assert_eq!(got.len(), want.len(), "{got:?} != {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert_span((g.0, g.1), *w);
    }
}

#[test]
fn edits_merge_annotations_of_a_kind_they_reach() {
    use kymo_core::Origin::File;
    // BAD from the file, overlapping, from 22 to 24 s and from 23 to 25 s.
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.channels.retain(|c| !c.trigger);
    spec.annotations = [(22.0, "BAD"), (23.0, "BAD"), (30.0, "x")]
        .map(|(onset, text)| kymo_testgen::Tal {
            onset,
            duration: Some(2.0),
            texts: vec![text.into()],
        })
        .into();
    let file = TempFile::written(&spec);
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    let at = |harness: &Harness<'_, KymoApp>, t| at_time(harness, t, 40.0);
    let create = |harness: &mut Harness<'_, KymoApp>, description: &str, from, to| {
        describe(harness, description);
        press(harness, Modifiers::NONE, Key::Escape);
        let (a, b) = (at(harness, from), at(harness, to));
        drag(harness, a, b);
    };
    create(&mut harness, "BAD", 2.0, 4.0);
    create(&mut harness, "BAD", 6.0, 8.0);
    create(&mut harness, "BAD", 10.0, 11.0);
    create(&mut harness, "x", 4.5, 5.5);
    assert_spans(
        spans(&harness, "BAD"),
        &[
            (2.0, 4.0),
            (6.0, 8.0),
            (10.0, 11.0),
            (22.0, 24.0),
            (23.0, 25.0),
        ],
    );
    // A new one reaching two of them becomes one with them; "x" stays apart.
    create(&mut harness, "BAD", 3.0, 7.0);
    let merged = [(2.0, 8.0), (10.0, 11.0), (22.0, 24.0), (23.0, 25.0)];
    assert_spans(spans(&harness, "BAD"), &merged);
    assert_span(selected_span(&harness), (2.0, 8.0));
    assert_spans(spans(&harness, "x"), &[(4.5, 5.5), (30.0, 32.0)]);
    // One step: undo brings all three back.
    press(&mut harness, command(), Key::Z);
    assert_spans(
        spans(&harness, "BAD"),
        &[
            (2.0, 4.0),
            (6.0, 8.0),
            (10.0, 11.0),
            (22.0, 24.0),
            (23.0, 25.0),
        ],
    );
    press(&mut harness, Modifiers::SHIFT | command(), Key::Z);
    assert_spans(spans(&harness, "BAD"), &merged);
    assert_span(selected_span(&harness), (2.0, 8.0));
    // A resize that reaches one merges as well.
    let (a, b) = (at(&harness, 8.0), at(&harness, 10.5));
    drag(&mut harness, a, b);
    assert_spans(
        spans(&harness, "BAD"),
        &[(2.0, 11.0), (22.0, 24.0), (23.0, 25.0)],
    );
    press(&mut harness, command(), Key::Z);
    assert_spans(spans(&harness, "BAD"), &merged);

    // The file's overlapping ones stay apart until one of them is edited: moved, it
    // merges with the other, keeping its origin.
    press(&mut harness, Modifiers::NONE, Key::Escape);
    press(&mut harness, Modifiers::ALT, Key::ArrowRight);
    assert_eq!(viewer(&harness).view().time(), 20.0..40.0);
    click_at_time(&mut harness, 22.5);
    assert_span(selected_span(&harness), (22.0, 24.0));
    // Dragged out and back, it is unchanged and merges nothing.
    let p = at(&harness, 22.5);
    harness.drag_at(p);
    harness.step();
    let q = at(&harness, 23.5);
    hover(&mut harness, q);
    hover(&mut harness, p);
    harness.drop_at(p);
    harness.step();
    assert_eq!(spans(&harness, "BAD").len(), 4);
    let (a, b) = (at(&harness, 22.5), at(&harness, 22.0));
    drag(&mut harness, a, b);
    let got = spans(&harness, "BAD");
    assert_spans(got.clone(), &[(2.0, 8.0), (10.0, 11.0), (21.5, 25.0)]);
    assert_eq!(got[2].2, File);
    assert_span(selected_span(&harness), (21.5, 25.0));
}

#[test]
fn merge_annotations_merges_the_shown_ones_in_one_step() {
    use kymo_core::Origin::File;
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.channels.retain(|c| !c.trigger);
    spec.annotations = [
        // A chain of three BAD, and one apart.
        (22.0, 2.0, "BAD"),
        (23.0, 2.0, "BAD"),
        (24.5, 1.5, "BAD"),
        (30.0, 1.0, "BAD"),
        // Two "x" that overlap.
        (22.0, 2.0, "x"),
        (23.5, 1.5, "x"),
    ]
    .map(|(onset, duration, text)| kymo_testgen::Tal {
        onset,
        duration: Some(duration),
        texts: vec![text.into()],
    })
    .into();
    let file = TempFile::written(&spec);
    let mut harness = window(file.path());
    ready(&mut harness);
    let bad = [(22.0, 24.0), (23.0, 25.0), (24.5, 26.0), (30.0, 31.0)];
    let x = [(22.0, 24.0), (23.5, 25.0)];

    // Enabled while some shown description has something to merge.
    let merge = "Merge Annotations";
    assert!(enabled(&mut harness, "Edit", merge));
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show BAD");
    assert!(enabled(&mut harness, "Edit", merge));
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show x");
    assert!(!enabled(&mut harness, "Edit", merge));
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show BAD");
    assert!(enabled(&mut harness, "Edit", merge));
    // Also through the legend's checkbox for all: mixed, it shows all, then hides all.
    for shown in [true, false] {
        click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Annotations");
        assert_eq!(enabled(&mut harness, "Edit", merge), shown);
    }
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show BAD");

    // With "x" hidden, only BAD merges: the chain becomes one, from the file still.
    choose(&mut harness, "Edit", merge);
    let merged = [(22.0, 26.0), (30.0, 31.0)];
    let got = spans(&harness, "BAD");
    assert_spans(got.clone(), &merged);
    assert!(got.iter().all(|s| s.2 == File));
    assert_spans(spans(&harness, "x"), &x);
    let count =
        |harness: &Harness<'_, KymoApp>, d| viewer(harness).legend().entry(d).unwrap().count;
    assert_eq!(count(&harness, "BAD"), 2);
    assert!(harness.state().title().contains("(edited)"));
    assert!(!enabled(&mut harness, "Edit", merge));

    // One step.
    press(&mut harness, command(), Key::Z);
    assert_spans(spans(&harness, "BAD"), &bad);
    assert!(!harness.state().title().contains("(edited)"));
    press(&mut harness, Modifiers::SHIFT | command(), Key::Z);
    assert_spans(spans(&harness, "BAD"), &merged);
    press(&mut harness, command(), Key::Z);

    // A selected annotation merged into another leaves that one selected.
    press(&mut harness, command(), Key::E);
    press(&mut harness, Modifiers::ALT, Key::ArrowRight);
    assert_eq!(viewer(&harness).view().time(), 20.0..40.0);
    click_at_time(&mut harness, 25.5);
    assert_span(selected_span(&harness), (24.5, 26.0));
    choose(&mut harness, "Edit", merge);
    assert_spans(spans(&harness, "BAD"), &merged);
    assert_span(selected_span(&harness), (22.0, 26.0));
    // Shown again, "x" merges too.
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show x");
    choose(&mut harness, "Edit", merge);
    assert_spans(spans(&harness, "x"), &[(22.0, 25.0)]);
    assert_span(selected_span(&harness), (22.0, 26.0));

    // It waits for a drag to end, even one that reaches another: moved over the BAD at
    // 30 s, the selected one merges only on release, as any edit does.
    let at = |harness: &Harness<'_, KymoApp>, t| at_time(harness, t, 40.0);
    let p = at(&harness, 24.0);
    hover(&mut harness, p);
    harness.drag_at(p);
    harness.step();
    let q = at(&harness, 28.5);
    hover(&mut harness, q);
    assert_spans(spans(&harness, "BAD"), &[(26.5, 30.5), (30.0, 31.0)]);
    let ctx = harness.ctx.clone();
    harness
        .state_mut()
        .perform(&ctx, kymo::action::Action::MergeAnnotations);
    assert_spans(spans(&harness, "BAD"), &[(26.5, 30.5), (30.0, 31.0)]);
    harness.drop_at(q);
    harness.step();
    assert_spans(spans(&harness, "BAD"), &[(26.5, 31.0)]);
}

#[test]
fn stim_annotations_never_merge() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    let stim = |harness: &Harness<'_, KymoApp>| {
        viewer(harness)
            .document()
            .annotations
            .iter()
            .filter(|a| a.data.origin == kymo_core::Origin::Stim)
            .map(|a| a.data.clone())
            .collect::<Vec<_>>()
    };
    let before = stim(&harness);
    let event = before
        .iter()
        .find(|a| (2.0..18.0).contains(&a.onset))
        .expect("a stim annotation in view")
        .clone();
    // A new annotation of its description over it.
    describe(&mut harness, &event.description);
    let (a, b) = (
        at_time(&harness, event.onset - 1.0, 40.0),
        at_time(&harness, event.onset + 1.0, 40.0),
    );
    drag(&mut harness, a, b);
    assert_eq!(stim(&harness), before);
    let d = selected_data(&harness).unwrap();
    assert_eq!(d.origin, kymo_core::Origin::Viewer);
    assert_span((d.onset, d.end()), (event.onset - 1.0, event.onset + 1.0));
}

/// The descriptions of the viewer's and the file's annotations, by onset.
fn descriptions(harness: &Harness<'_, KymoApp>) -> Vec<(f64, f64, String)> {
    viewer(harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin != kymo_core::Origin::Stim)
        .map(|a| (a.data.onset, a.data.end(), a.data.description.clone()))
        .collect()
}

#[test]
fn the_field_changes_the_selected_annotations_description() {
    // A file annotation "trigger 3" from 15.5 to 16.5 s, and events.
    let file = without_triggers("bdf+c-small");
    let name = file
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut harness = window(file.path());
    ready(&mut harness);
    assert_eq!(harness.state().title(), format!("{name} – Kymo"));
    assert_eq!(harness.state().window_title(), harness.state().title());
    press(&mut harness, command(), Key::E);
    let at = |harness: &Harness<'_, KymoApp>, t| at_time(harness, t, 40.0);
    let (a, b) = (at(&harness, 2.0), at(&harness, 4.0));
    drag(&mut harness, a, b);
    assert_eq!(harness.state().title(), format!("{name} (edited) – Kymo"));
    assert_eq!(harness.state().window_title(), harness.state().title());
    let field = "Description of the selected annotation";
    assert_eq!(harness.get_by_label(field).value().as_deref(), Some("BAD"));
    let first = |harness: &Harness<'_, KymoApp>| made(harness)[0].2.clone();
    let retype = |harness: &mut Harness<'_, KymoApp>, text: &str| {
        harness.get_by_label(field).focus();
        harness.run_steps(2);
        press(harness, command(), Key::A);
        harness.get_by_label(field).type_text(text);
        harness.run_steps(2);
    };

    // Typed, it applies on Enter, not before: no legend entry per letter.
    retype(&mut harness, "eye");
    assert_eq!(first(&harness), "BAD");
    assert!(viewer(&harness).legend().entry("e").is_none());
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert_eq!(first(&harness), "eye");
    assert_eq!(viewer(&harness).legend().entry("eye").unwrap().count, 1);
    assert!(
        viewer(&harness)
            .legend()
            .entries()
            .all(|e| e.description != "BAD")
    );
    // New annotations keep theirs.
    assert_eq!(viewer(&harness).description(), "BAD");
    // One step.
    press(&mut harness, command(), Key::Z);
    assert_eq!(first(&harness), "BAD");
    assert_eq!(harness.get_by_label(field).value().as_deref(), Some("BAD"));
    press(&mut harness, command() | Modifiers::SHIFT, Key::Z);
    assert_eq!(first(&harness), "eye");
    // Esc in the field takes back the typing, and keeps the selection.
    retype(&mut harness, "zzz");
    press(&mut harness, Modifiers::NONE, Key::Escape);
    harness.run_steps(2);
    assert_eq!(first(&harness), "eye");
    assert_eq!(harness.get_by_label(field).value().as_deref(), Some("eye"));
    assert!(viewer(&harness).selected().is_some());
    // An empty field changes nothing.
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    press(&mut harness, command(), Key::A);
    press(&mut harness, Modifiers::NONE, Key::Backspace);
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert_eq!(first(&harness), "eye");
    assert_eq!(harness.get_by_label(field).value().as_deref(), Some("eye"));

    // A click on another annotation applies the typing to the one it was typed for.
    retype(&mut harness, "blink");
    click_at_time(&mut harness, 16.0);
    harness.run_steps(2);
    assert_eq!(first(&harness), "blink");
    assert_eq!(selected_data(&harness).unwrap().description, "trigger 3");
    assert_eq!(
        harness.get_by_label(field).value().as_deref(),
        Some("trigger 3")
    );
    // Picked from the menu, it applies at once, to a file annotation as well.
    harness.get_by_label("Descriptions").click();
    harness.run_steps(2);
    // Below the menu's fold: the trigger descriptions come first.
    click_in_list(&mut harness, egui::accesskit::Role::Button, "blink");
    harness.run_steps(2);
    let d = selected_data(&harness).unwrap();
    assert_eq!(
        (d.description.as_str(), d.origin),
        ("blink", kymo_core::Origin::File)
    );
    press(&mut harness, command(), Key::Z);
    assert_eq!(selected_data(&harness).unwrap().description, "trigger 3");

    // Renamed into a kind it reaches, it merges, and a hidden description shows again.
    describe(&mut harness, "x");
    let (a, b) = (at(&harness, 3.0), at(&harness, 5.0));
    drag(&mut harness, a, b);
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show blink");
    assert!(!viewer(&harness).legend().entry("blink").unwrap().shown);
    retype(&mut harness, "blink");
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert!(viewer(&harness).legend().entry("blink").unwrap().shown);
    let blinks: Vec<_> = descriptions(&harness)
        .into_iter()
        .filter(|d| d.2 == "blink")
        .collect();
    assert_eq!(blinks.len(), 1, "{blinks:?}");
    assert_span((blinks[0].0, blinks[0].1), (2.0, 5.0));
    assert_span(selected_span(&harness), (2.0, 5.0));

    // Undoing every edit takes the title back; redoing one marks it again.
    while viewer(&harness).history().can_undo() {
        press(&mut harness, command(), Key::Z);
    }
    assert_eq!(harness.state().title(), format!("{name} – Kymo"));
    press(&mut harness, command() | Modifiers::SHIFT, Key::Z);
    assert_eq!(harness.state().title(), format!("{name} (edited) – Kymo"));
    // Closed, the title is the app's.
    press(&mut harness, command(), Key::W);
    harness.get_by_label("Don't Save").click();
    harness.run_steps(2);
    assert_eq!(harness.state().title(), "Kymo");
    assert_eq!(harness.state().window_title(), "Kymo");
}

/// The viewer's annotations (not the file's or stim ones) as onset, end and description.
fn viewer_spans(harness: &Harness<'_, KymoApp>) -> Vec<(f64, f64, String)> {
    viewer(harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin == kymo_core::Origin::Viewer)
        .map(|a| (a.data.onset, a.data.end(), a.data.description.clone()))
        .collect()
}

#[test]
fn saved_annotations_load_into_another_window() {
    // 12 file annotations, "trigger 0" to "trigger 11"; the recording starts at
    // 2024-01-02 03:04:05, its first sample at 0.
    let file = without_triggers("bdf+c-small");
    let dir = TempDir::new();
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    let (a, b) = (at_time(&harness, 2.0, 40.0), at_time(&harness, 4.0, 40.0));
    drag(&mut harness, a, b);
    let made = viewer_spans(&harness);
    // The dialog's name gets `.tsv`: nothing reads it as another format.
    harness
        .state_mut()
        .answer_file_dialogs([Some(dir.path().join("saved"))]);
    press(&mut harness, command(), Key::S);
    let saved = dir.path().join("saved.tsv");
    let text = std::fs::read_to_string(&saved).unwrap();
    assert_eq!(text.lines().next(), Some("onset\tduration\ttrial_type"));
    assert_eq!(text.lines().count(), 1 + 13);
    assert!(
        text.contains("\ntrigger 3\t") || text.contains("\ttrigger 3\n"),
        "{text}"
    );
    assert!(!harness.state().title().contains("(edited)"));
    // Saving again suggests the same file.
    assert_eq!(viewer(&harness).save_path(), saved);
    drop(harness);

    // In a new window, loaded as the viewer's, as one step.
    let mut harness = window(file.path());
    ready(&mut harness);
    harness
        .state_mut()
        .answer_file_dialogs([Some(saved.clone())]);
    choose(&mut harness, "File", "Load Annotations…");
    assert!(harness.state().notice().is_none());
    let loaded = viewer_spans(&harness);
    assert_eq!(loaded.len(), 13);
    assert!(loaded.contains(&made[0]), "{loaded:?}");
    assert!(harness.state().title().contains("(edited)"));
    // The legend counts them.
    assert_eq!(viewer(&harness).legend().entry("BAD").unwrap().count, 1);
    assert_eq!(
        viewer(&harness).legend().entry("trigger 3").unwrap().count,
        2
    );
    press(&mut harness, command(), Key::Z);
    assert!(viewer_spans(&harness).is_empty());
    assert!(
        viewer(&harness)
            .legend()
            .entries()
            .all(|e| e.description != "BAD")
    );
    assert!(!harness.state().title().contains("(edited)"));

    // MNE's CSV: dates and times; another day's annotation is skipped, and said so.
    let csv = dir.path().join("mne.csv");
    std::fs::write(
        &csv,
        "onset,duration,description\n\
         2024-01-02 03:04:15.500,1.0,from MNE\n\
         2024-01-03 00:00:00.000,1.0,elsewhere\n",
    )
    .unwrap();
    assert!(harness.state_mut().load_annotations_from(&csv));
    assert_eq!(viewer_spans(&harness), [(10.5, 11.5, "from MNE".into())]);
    let (title, text) = harness.state().notice().unwrap();
    assert_eq!(title, "Annotations loaded");
    assert!(
        text.contains("Loaded 1 annotation from mne.csv. Skipped 1 outside"),
        "{text}"
    );
    harness.run_steps(2);
    harness.get_by_label("OK").click();
    harness.run_steps(2);
    assert!(harness.state().notice().is_none());

    // A broken file adds nothing, and says where it broke.
    let broken = dir.path().join("broken.tsv");
    std::fs::write(&broken, "onset\tduration\ttrial_type\n1\tlong\tx\n").unwrap();
    assert!(!harness.state_mut().load_annotations_from(&broken));
    let (_, text) = harness.state().notice().unwrap();
    assert!(
        text.contains("broken.tsv: line 2: duration \"long\""),
        "{text}"
    );
    assert_eq!(viewer_spans(&harness).len(), 1);
    // As does another kind of file; Esc dismisses the notice, and nothing else.
    harness.run_steps(2);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert!(harness.state().notice().is_none());
    assert!(!harness.state_mut().load_annotations_from(file.path()));
    assert!(
        harness
            .state()
            .notice()
            .unwrap()
            .1
            .contains("not a .tsv or .csv file")
    );
}

#[test]
fn export_writes_the_descriptions_shown() {
    // File annotations "trigger 0" to "trigger 11", and stim annotations.
    let file = TempFile::preset("bdf+c-small");
    let dir = TempDir::new();
    let mut harness = window(file.path());
    ready(&mut harness);
    let stim = viewer(&harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin == kymo_core::Origin::Stim)
        .count();
    assert!(stim > 0);
    press(&mut harness, command(), Key::E);
    let (a, b) = (at_time(&harness, 2.0, 40.0), at_time(&harness, 4.0, 40.0));
    drag(&mut harness, a, b);
    click_in_list(
        &mut harness,
        egui::accesskit::Role::CheckBox,
        "Show trigger 3",
    );
    let export = dir.path().join("shown.tsv");
    harness
        .state_mut()
        .answer_file_dialogs([Some(export.clone())]);
    choose(&mut harness, "File", "Export Shown Annotations…");
    let text = std::fs::read_to_string(&export).unwrap();
    // 11 of the file's, the new one, and the stim ones.
    assert_eq!(text.lines().count(), 1 + 11 + 1 + stim, "{text}");
    assert!(!text.contains("trigger 3"));
    assert!(text.contains("\tBAD"));
    // Not a save.
    assert!(harness.state().title().contains("(edited)"));
    // Saving writes them all, stim ones apart.
    let saved = dir.path().join("all.tsv");
    harness
        .state_mut()
        .answer_file_dialogs([Some(saved.clone())]);
    choose(&mut harness, "File", "Save Annotations…");
    let text = std::fs::read_to_string(&saved).unwrap();
    assert_eq!(text.lines().count(), 1 + 12 + 1, "{text}");

    // With every description hidden there is nothing to export, until one shows again.
    assert!(enabled(&mut harness, "File", "Export Shown"));
    // Mixed, as trigger 3 is hidden: the first click shows all, the second hides all.
    for _ in 0..2 {
        click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Annotations");
    }
    assert!(!enabled(&mut harness, "File", "Export Shown"));
    click_in_list(
        &mut harness,
        egui::accesskit::Role::CheckBox,
        "Show trigger 3",
    );
    assert!(enabled(&mut harness, "File", "Export Shown"));
}

#[test]
fn unsaved_changes_are_asked_about() {
    let file = without_triggers("bdf+c-small");
    let other = TempFile::preset("bdf+c-small");
    let dir = TempDir::new();
    let mut harness = window(file.path());
    ready(&mut harness);
    // Without changes, Close closes.
    press(&mut harness, command(), Key::W);
    assert_eq!(harness.state().title(), "Kymo");

    let edit = |harness: &mut Harness<'_, KymoApp>| {
        harness
            .state_mut()
            .answer_file_dialogs([Some(file.path().to_owned())]);
        press(harness, command(), Key::O);
        ready(harness);
        press(harness, command(), Key::E);
        let (a, b) = (at_time(harness, 2.0, 40.0), at_time(harness, 4.0, 40.0));
        drag(harness, a, b);
        assert!(harness.state().title().contains("(edited)"));
    };
    let answer = |harness: &mut Harness<'_, KymoApp>, button: &str| {
        harness.step();
        harness.get_by_label(button).click();
        harness.run_steps(2);
    };
    edit(&mut harness);
    // Cancel, and Esc, keep it open; keys wait for the answer.
    press(&mut harness, command(), Key::W);
    assert_eq!(harness.state().leaving(), Some(kymo::app::Leave::Close));
    harness.get_by_label_contains("Save the changes to the annotations of");
    answer(&mut harness, "Cancel");
    assert_eq!(harness.state().leaving(), None);
    assert!(harness.state().title().contains("(edited)"));
    press(&mut harness, command(), Key::W);
    press(&mut harness, command(), Key::Z);
    assert_eq!(viewer_spans(&harness).len(), 1);
    harness.step();
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(harness.state().leaving(), None);
    assert!(viewer(&harness).selected().is_some());
    // Save… with the save dialog canceled stays; saved, it closes.
    press(&mut harness, command(), Key::W);
    harness.state_mut().answer_file_dialogs([None]);
    answer(&mut harness, "Save…");
    assert!(harness.state().title().contains("(edited)"));
    let saved = dir.path().join("kept.tsv");
    press(&mut harness, command(), Key::W);
    harness
        .state_mut()
        .answer_file_dialogs([Some(saved.clone())]);
    answer(&mut harness, "Save…");
    assert_eq!(harness.state().title(), "Kymo");
    assert_eq!(
        std::fs::read_to_string(&saved).unwrap().lines().count(),
        1 + 13
    );

    // Opening another: Don't Save goes on to the dialog.
    edit(&mut harness);
    press(&mut harness, command(), Key::O);
    assert_eq!(harness.state().leaving(), Some(kymo::app::Leave::Open));
    harness
        .state_mut()
        .answer_file_dialogs([Some(other.path().to_owned())]);
    answer(&mut harness, "Don't Save");
    ready(&mut harness);
    assert_eq!(viewer(&harness).document().path, other.path());

    // Quitting, from the menu or the window's close button.
    edit(&mut harness);
    press(&mut harness, command(), Key::Q);
    assert_eq!(harness.state().leaving(), Some(kymo::app::Leave::Quit));
    assert!(!harness.state().closing());
    answer(&mut harness, "Cancel");
    harness
        .input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    harness.step();
    assert_eq!(harness.state().leaving(), Some(kymo::app::Leave::Quit));
    answer(&mut harness, "Don't Save");
    assert!(harness.state().closing());
}

/// The sidebar lists every type the recording has, in the sidebar's order, each in its
/// color; Settings has a color swatch for every type, three to a row in the same order.
#[test]
fn every_channel_type_has_a_row_a_color_and_a_swatch() {
    use egui::accesskit::Role;
    use kymo::ui::header::kind_name;
    let file = TempFile::preset("edf+c-kinds");
    let mut harness = screenshot_window(file.path());
    ready(&mut harness);
    // Top to bottom, the types of edf+c-kinds: brain signals, other body signals, stim;
    // none it lacks.
    let present = [
        "MEG mag", "MEG grad", "EEG", "sEEG", "ECoG", "DBS", "EOG", "ECG", "EMG", "Resp", "Bio",
        "Stim",
    ];
    let tops: Vec<f32> = present
        .iter()
        .map(|name| {
            harness
                .get_by_role_and_label(Role::CheckBox, name)
                .rect()
                .top()
        })
        .collect();
    assert!(tops.is_sorted_by(|a, b| a < b), "{tops:?}");
    for absent in ["MEG ref", "Misc"] {
        assert!(
            harness
                .query_by_role_and_label(Role::CheckBox, absent)
                .is_none()
        );
    }
    // Each row's name in its type's color.
    let image = harness.render().unwrap();
    let visuals = harness.ctx.global_style().visuals.clone();
    for kind in [
        ChannelKind::Mag,
        ChannelKind::Seeg,
        ChannelKind::Dbs,
        ChannelKind::Eog,
    ] {
        let r = harness
            .get_by_role_and_label(Role::CheckBox, kind_name(kind))
            .rect();
        let color = trace_color(kind, &visuals);
        let colored = (r.left() as u32..r.right() as u32)
            .flat_map(|x| (r.top() as u32..r.bottom() as u32).map(move |y| (x, y)))
            .filter(|&(x, y)| near(&image.get_pixel(x, y).0, color))
            .count();
        assert!(colored > 10, "{kind:?}: {colored} pixels of {color:?}");
    }

    harness.key_press_modifiers(command(), Key::Comma);
    harness.run_steps(2);
    let all = [
        "MEG mag", "MEG grad", "EEG", "sEEG", "ECoG", "DBS", "EOG", "ECG", "EMG", "Resp", "Bio",
        "MEG ref", "Stim", "Misc",
    ];
    assert_eq!(all.len(), KINDS.len());
    let swatches: Vec<egui::Rect> = all
        .iter()
        .map(|name| harness.get_by_label(&format!("Color of {name}")).rect())
        .collect();
    for (i, pair) in swatches.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        if (i + 1) % 3 == 0 {
            assert!(
                b.top() > a.bottom() && b.left() < a.left(),
                "row break after {i}"
            );
        } else {
            assert!(
                b.left() > a.right() && b.top() == a.top(),
                "{i} and {}",
                i + 1
            );
        }
    }
}

/// Settings lists how it looks first, then how a recording opens, then memory.
#[test]
fn settings_put_appearance_first_and_memory_last() {
    use egui::accesskit::Role;
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.key_press_modifiers(command(), Key::Comma);
    harness.run_steps(2);
    let tops: Vec<f32> = [
        "Theme",
        "Colors",
        "Traces",
        "Default window",
        "Scale",
        "Tile cache",
        "GPU memory",
    ]
    .iter()
    .map(|label| {
        harness
            .get_by_role_and_label(Role::Label, label)
            .rect()
            .top()
    })
    .collect();
    assert!(tops.is_sorted(), "{tops:?}");
}

/// The sidebar has no Open button: File → Open and ⌘O open files, and the empty window
/// keeps its button.
#[test]
fn the_sidebar_has_no_open_button() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    assert!(harness.query_by_label("Open…").is_none());
    harness.key_press_modifiers(command(), Key::W);
    harness.run_steps(2);
    harness.get_by_label("Open…");
}

/// A recording with 60 channels, more than the channel list shows at once.
fn many_channels() -> TempFile {
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    let status = spec.channels.pop().unwrap();
    for k in 0..55 {
        spec.channels.push(kymo_testgen::ChannelSpec::eeg(
            format!("E{k}"),
            512,
            spec.format,
        ));
    }
    spec.channels.push(status);
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    file
}

/// Another recording's channel list starts at its top, not where the last one was.
#[test]
fn the_channel_list_starts_at_the_top_for_each_recording() {
    use egui::accesskit::Role;
    let (first, second) = (many_channels(), many_channels());
    let mut harness = window(first.path());
    ready(&mut harness);
    let a1 = harness
        .get_by_role_and_label(Role::Label, "A1")
        .rect()
        .center();
    harness.hover_at(a1);
    for _ in 0..10 {
        scroll(&mut harness, egui::vec2(0.0, -2000.0), Modifiers::NONE);
    }
    harness.run_steps(2);
    assert!(harness.query_by_label("A1 bad").is_none(), "not scrolled");

    let ctx = harness.ctx.clone();
    harness.state_mut().open(&ctx, second.path().to_owned());
    ready(&mut harness);
    assert!(harness.query_by_label("A1 bad").is_some(), "still scrolled");
}
