//! The window, driven headless through egui_kittest: opening, keys, clicks and the status
//! line, with the trace renderer on a real GPU where there is one.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, Modifiers};
use eframe::egui_wgpu;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use kymo::action::Action;
use kymo::annotations::{AnnotationColor, AnnotationColors};
use kymo::app::{KymoApp, Menus, Options};
use kymo::keymap::Platform;
use kymo::settings::{Settings, load_annotation_colors};
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
    Harness::builder().with_size(SIZE).build_eframe(|cc| {
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

/// Whether the status line reads a value in uV; the annotations under the pointer may
/// follow it.
fn has_reading(status: &str) -> bool {
    status.split(" · ").any(|part| part.ends_with(" uV"))
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
fn names_and_b_toggle_bad_channels() {
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

    // B toggles the channel under the pointer, over the traces too.
    hover(
        &mut harness,
        egui::pos2(layout.traces.center().x, row_y(1.0)),
    );
    assert_eq!(viewer(&harness).hover().channel, Some(a2));
    harness.key_press_modifiers(command(), Key::B);
    harness.step();
    assert!(!viewer(&harness).view().is_bad(a2));
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
    assert!(left.ends_with(&format!(" uV · {code}")), "{left}");
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
    // The pointer is on the menu, not over a channel.
    assert!(!enabled(&mut harness, "View", "Toggle Bad"));
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
    assert_eq!(viewer(&harness).view().time(), 5.0..15.0);
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

/// The numbers of the status line's reading ("12.5 uV" or "−3 … 4 uV").
fn reading_numbers(status: &str) -> Vec<f64> {
    let reading = status.rsplit_once(" A1 ").unwrap().1;
    // Up to the annotations under the pointer, if any.
    let reading = reading.split(" · ").next().unwrap();
    reading
        .trim_end_matches(" uV")
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
    let uv = column_left(&harness, Role::Label, "uV");

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
    assert_eq!(column_left(&harness, Role::Label, "uV"), uv);
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
    assert_eq!(rights.len(), 7, "{rights:?}");
    assert!(rights.iter().all(|r| r.1 == rights[0].1), "{rights:?}");
    let lefts: Vec<f32> = ["64 Hz", "256 Hz"]
        .iter()
        .map(|rate| harness.get_all_by_label(rate).next().unwrap().rect().left())
        .collect();
    assert!(lefts[0] > lefts[1], "{lefts:?}");
    assert!(harness.query_by_label("TRIGGER").is_some());
    assert!(harness.query_by_label("Boolean").is_none());
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
    let status = harness
        .get_by_label(&viewer(&harness).status().right)
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
    let mut harness = window(file.path());
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
        let mut harness = window(file.path());
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
    harness.get_by_label("Pinch");
    harness.key_press_modifiers(command(), Key::Slash);
    harness.step();
    assert!(!harness.state().shortcuts_open());
    assert!(harness.query_by_label("Pinch").is_none());

    choose(&mut harness, "Help", "About Kymo");
    assert!(harness.state().about_open());
    harness.get_by_label(kymo::ui::menus::ABOUT);
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

    let mut fresh = gpu_window(second.path(), setup.clone());
    ready(&mut fresh);
    let want = traces_image(&mut fresh);

    let mut switched = gpu_window(first.path(), setup);
    ready(&mut switched);
    let before = traces_image(&mut switched);
    assert_ne!(before, want, "the two recordings look the same");
    let ctx = switched.ctx.clone();
    switched.state_mut().open(&ctx, second.path().to_owned());
    ready(&mut switched);
    assert!(
        traces_image(&mut switched) == want,
        "tiles of the first recording drawn"
    );
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
