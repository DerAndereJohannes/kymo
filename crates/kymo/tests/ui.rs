//! The window, driven headless through egui_kittest: opening, keys, clicks and the status
//! line, with the trace renderer on a real GPU where there is one.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, Modifiers};
use eframe::egui_wgpu;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use kymo::app::{KymoApp, Menus, Options};
use kymo::keymap::Platform;
use kymo::settings::Settings;
use kymo::ui::gpu::Gpu;
use kymo::ui::palette::trace_color;
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

/// Pyramids in memory, the menu bar drawn by egui as on Linux, on every platform, and
/// default settings.
fn options() -> Options {
    Options {
        sidecar_dir: None,
        menus: Menus::Egui(Platform::Linux),
        settings: Settings::default(),
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
    harness.key_press_modifiers(command(), Key::B);
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
    let mut harness = window(&file.0);
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
    let mut harness = window(&file.0);
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
    kymo_testgen::write(&spec, &file.0).unwrap();
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

/// Whether every visible channel's baseline is its brute-force mean over the view.
fn baselines_are_window_means(app: &KymoApp) -> bool {
    let viewer = app.viewer().unwrap();
    let view = viewer.view();
    let rec = &viewer.document().recording;
    view.visible_channels().iter().all(|&c| {
        let want = window_mean(rec, c, view.time()).unwrap();
        // The brute force reads f32 values.
        (viewer.baseline(c) - want).abs() <= 1e-5 * want.abs().max(1.0)
    })
}

/// The numbers of the status line's reading ("12.5 uV" or "−3 … 4 uV").
fn reading_numbers(status: &str) -> Vec<f64> {
    let reading = status.rsplit_once(" A1 ").unwrap().1;
    reading
        .trim_end_matches(" uV")
        .split(" … ")
        .map(|x| x.replace('−', "-").parse().unwrap())
        .collect()
}

#[test]
fn dc_removal_centres_each_trace_on_its_window_mean() {
    let file = offset_preset("bdf+c-small", -21_000.0);
    let mut harness = window(&file.0);
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
        app.viewer().unwrap().status().left.ends_with(" uV")
    });
    let raw = reading_numbers(&viewer(&harness).status().left);
    assert!(raw.iter().all(|&x| x < -20_000.0), "{raw:?}");

    harness.key_press_modifiers(command(), Key::D);
    harness.step();
    assert!(viewer(&harness).view().dc_removed());
    step_until(&mut harness, "the means", baselines_are_window_means);
    let mean = viewer(&harness).baseline(a1);
    assert!((-21_100.0..-20_900.0).contains(&mean), "{mean}");
    let centred = reading_numbers(&viewer(&harness).status().left);
    assert_eq!(centred.len(), raw.len());
    for (r, c) in raw.iter().zip(&centred) {
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

/// bdf+c-small with its channels relabelled to five types: A1 and A2 (EEG), "EOG L",
/// "ECG" and Status (stim).
fn typed_preset() -> TempFile {
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.channels[2].label = "EOG L".into();
    spec.channels[3].label = "ECG".into();
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, &file.0).unwrap();
    file
}

/// Clicks the sidebar widget labelled `label` (exactly, with `role`).
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
    let mut harness = window(&file.0);
    ready(&mut harness);
    assert_eq!(shown(&harness), [0, 1, 2, 3, 4]);
    for kind in ["EEG", "EOG", "ECG", "stim"] {
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

#[test]
fn the_sidebar_hides_and_shows() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(&file.0);
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

/// Sets the number field labelled `label` in the Settings window.
fn set_field(harness: &mut Harness<'_, KymoApp>, label: &str, value: f64) {
    harness
        .get_by_role_and_label(egui::accesskit::Role::SpinButton, label)
        .set_value_accesskit(value);
    harness.run_steps(2);
}

#[test]
fn settings_apply_at_once() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(&file.0);
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
fn settings_are_kept_between_runs() {
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

    let file = TempFile::preset("bdf+c-small");
    let mut storage = Memory::default();
    {
        let mut harness = window(&file.0);
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
    let mut harness = window_with(&file.0, settings);
    ready(&mut harness);
    assert_eq!(viewer(&harness).view().time(), 0.0..8.0);
    assert_eq!(viewer(&harness).cache().budget(), 300 << 20);
    assert!(!harness.state().settings_open());
}

#[test]
fn the_shortcut_overview_and_about_open() {
    let file = TempFile::new("bdf");
    // A missing file: both work without a recording.
    let mut harness = window(&file.0);
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

#[test]
fn dc_removal_brings_an_offset_trace_into_its_row() {
    let Some(setup) = gpu_setup() else { return };
    let file = offset_preset("bdf+c-small", -21_000.0);
    let mut harness = gpu_window(&file.0, setup);
    ready(&mut harness);
    let trace = harness.ctx.global_style().visuals.text_color();
    // Pixels of the first row (A1) in the trace colour.
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
    let mut harness = gpu_window(&file.0, setup);
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
fn traces_take_their_types_colours() {
    let Some(setup) = gpu_setup() else { return };
    // The BDF Status channel holds status bits around 2^20, far off its row; a flat
    // EEG signal labelled TRIGGER stands in for stim.
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.channels[1].label = "EOG L".into();
    spec.channels[2].label = "ECG".into();
    spec.channels[3].label = "TRIGGER".into();
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, &file.0).unwrap();
    let mut harness = gpu_window(&file.0, setup);
    ready(&mut harness);
    let visuals = harness.ctx.global_style().visuals.clone();
    // The rows to check, top to bottom; Status (also stim) is fifth.
    let kinds = [
        kymo_core::ChannelKind::Eeg,
        kymo_core::ChannelKind::Eog,
        kymo_core::ChannelKind::Ecg,
        kymo_core::ChannelKind::Stim,
    ];
    let colors: Vec<_> = kinds.iter().map(|&k| trace_color(k, &visuals)).collect();
    // Per row, the pixels in each type's colour.
    let counts = |harness: &mut Harness<'_, KymoApp>| {
        let traces = viewer(harness).layout().unwrap().traces;
        let width = traces.width() as usize;
        let row_height = traces.height() as usize / 5;
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

    // A bad channel is greyed out, whatever its type.
    harness.get_by_label("ECG bad").click();
    harness.run_steps(2);
    let after = counts(&mut harness);
    assert!(after[2][2] < before[2][2] / 20, "{:?}", after[2]);
}

/// Whether a pixel is within a small distance of `color`.
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
    let path = std::env::var_os("KYMO_UI_FILE").map_or(generated.0.clone(), PathBuf::from);
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
