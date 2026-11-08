//! Frame times, step latency and peak memory of the viewer on a recording, for the phase 2
//! measurements (`docs/phase2-plan.md`, step 7). It drives the real window through kymo's
//! public API and egui's input, as the keys and the trackpad do:
//!
//! ```text
//! cargo bench -p kymo --bench frames -- FILE [--fresh]
//! ```
//!
//! The window (1280×800, vsync off, drawing continuously) opens FILE at the default view
//! (20 s, 20 channels) and waits until it is complete. Then, in steps, each followed by
//! frames until the view is complete (no tile drawn from a coarser one) or [`STEP_LIMIT`]
//! has passed:
//!
//! - **pan:** 40 × Pan Right, a quarter window each;
//! - **zoom out, zoom in:** Zoom Out (×2) until the whole recording shows, and as many Zoom In;
//! - **page down, page up:** Channel Page Down until the last channel shows, and as many
//!   Channel Page Up.
//!
//! And continuously, one input per frame without waiting, as on a trackpad:
//!
//! - **glide:** [`GLIDE_FRAMES`] scrolls of 1/100 of the window to later times;
//! - **pinch out, pinch in:** pinches of 2^(1/16) around the middle of the traces until the
//!   whole recording shows, and as many back.
//!
//! Without `--fresh` the sidecar in the user's cache directory is used, built first (and
//! then the benchmark started again, so the build doesn't count towards peak memory); with
//! `--fresh` the pyramid builds into an empty directory during the run, as on a first open.

// A measuring tool: panicking on a broken input is the point.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Event, MouseWheelSource, MouseWheelUnit, Pos2, TouchPhase, Ui, Vec2};
use kymo::action::Action;
use kymo::app::{KymoApp, Menus, Options};
use kymo::keymap::Platform;
use kymo::settings::Settings;
use kymo::ui::gpu::Gpu;
use kymo::view::ViewState;
use kymo_core::pyramid::Sidecar;

/// How long a step may wait for a complete view.
const STEP_LIMIT: Duration = Duration::from_secs(2);
/// How long the first view may take to complete.
const OPEN_LIMIT: Duration = Duration::from_secs(30);
const PAN_STEPS: usize = 40;
const GLIDE_FRAMES: usize = 300;
/// Pinches per doubling of the window.
const PINCHES_PER_DOUBLING: f32 = 16.0;

fn main() {
    // `cargo bench` passes `--bench`.
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let fresh = args.iter().any(|a| a == "--fresh");
    let files: Vec<PathBuf> = args
        .iter()
        .filter(|a| *a != "--fresh")
        .map(PathBuf::from)
        .collect();
    let [path] = &files[..] else {
        if args.is_empty() {
            eprintln!("skipping: run `cargo bench -p kymo --bench frames -- FILE [--fresh]`");
            return;
        }
        eprintln!("usage: cargo bench -p kymo --bench frames -- FILE [--fresh]");
        std::process::exit(2);
    };

    let fresh_dir = fresh.then(|| {
        let dir = std::env::temp_dir().join(format!("kymo-bench-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    });
    let sidecar_dir = match &fresh_dir {
        Some(dir) => dir.clone(),
        None => {
            let dir = Sidecar::default_dir().expect("no cache directory");
            if build_sidecar(&dir, path) {
                // Measure in a process that hasn't built anything.
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(&args)
                    .status()
                    .unwrap();
                std::process::exit(status.code().unwrap_or(1));
            }
            dir
        }
    };

    let report = run(path, sidecar_dir);
    if let Some(dir) = fresh_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    let settings = Settings::default();
    println!("{}", path.display());
    println!(
        "sidecar: {}",
        if fresh {
            "built during the run"
        } else {
            "complete"
        }
    );
    report.print();
    match peak_resident_bytes() {
        Some(bytes) => println!("peak resident memory: {:.0} MB", bytes as f64 / 1e6),
        None => println!("peak resident memory: not measured on this platform"),
    }
    if let Some(bytes) = peak_footprint_bytes() {
        println!(
            "peak physical footprint: {:.0} MB (without file pages the system can drop)",
            bytes as f64 / 1e6
        );
    }
    println!(
        "budgets: tile cache {:.0} MB, GPU {:.0} MB",
        settings.tile_cache_bytes() as f64 / 1e6,
        settings.gpu_memory_bytes() as f64 / 1e6
    );
}

/// Builds the sidecar of `path` in `dir` unless it is complete; whether it built one.
fn build_sidecar(dir: &Path, path: &Path) -> bool {
    std::fs::create_dir_all(dir).unwrap();
    let document = kymo::document::open(path).unwrap();
    let sidecar = Sidecar::open_or_create(dir, path, &document.recording).unwrap();
    if sidecar.is_complete() {
        return false;
    }
    eprintln!("building the sidecar first (not measured)…");
    assert!(sidecar.build(&document.recording).unwrap());
    true
}

/// Opens the window, runs the sequence and returns what it measured.
fn run(path: &Path, sidecar_dir: PathBuf) -> Report {
    let report = Rc::new(RefCell::new(Report::default()));
    let out = Rc::clone(&report);
    let path = path.to_owned();
    let mut native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("kymo bench")
            .with_inner_size([1280.0, 800.0])
            .with_resizable(false),
        persist_window: false,
        ..Default::default()
    };
    native.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
    eframe::run_native(
        "kymo-bench",
        native,
        Box::new(move |cc| {
            let gpu = Gpu::new(cc.wgpu_render_state.as_ref());
            let options = Options {
                sidecar_dir: Some(sidecar_dir),
                menus: Menus::Egui(Platform::current()),
                settings: Settings::default(),
            };
            let app = KymoApp::new(&cc.egui_ctx, gpu, options, Some(path));
            Ok(Box::new(Bench::new(app, out)))
        }),
    )
    .unwrap();
    report.take()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Opening,
    Pan,
    ZoomOut,
    ZoomIn,
    PageDown,
    PageUp,
    Glide,
    PinchOut,
    PinchIn,
    Done,
}

impl Phase {
    const MEASURED: [Phase; 8] = [
        Phase::Pan,
        Phase::ZoomOut,
        Phase::ZoomIn,
        Phase::PageDown,
        Phase::PageUp,
        Phase::Glide,
        Phase::PinchOut,
        Phase::PinchIn,
    ];

    fn name(self) -> &'static str {
        match self {
            Phase::Opening => "opening",
            Phase::Pan => "pan",
            Phase::ZoomOut => "zoom out",
            Phase::ZoomIn => "zoom in",
            Phase::PageDown => "page down",
            Phase::PageUp => "page up",
            Phase::Glide => "glide",
            Phase::PinchOut => "pinch out",
            Phase::PinchIn => "pinch in",
            Phase::Done => "done",
        }
    }

    /// One input per frame, without waiting for a complete view.
    fn continuous(self) -> bool {
        matches!(self, Phase::Glide | Phase::PinchOut | Phase::PinchIn)
    }

    fn after(self) -> Phase {
        match self {
            Phase::Opening => Phase::Pan,
            Phase::Pan => Phase::ZoomOut,
            Phase::ZoomOut => Phase::ZoomIn,
            Phase::ZoomIn => Phase::PageDown,
            Phase::PageDown => Phase::PageUp,
            Phase::PageUp => Phase::Glide,
            Phase::Glide => Phase::PinchOut,
            Phase::PinchOut => Phase::PinchIn,
            Phase::PinchIn | Phase::Done => Phase::Done,
        }
    }
}

/// What a step does.
#[derive(Clone, Copy, Debug)]
enum Input {
    Key(Action),
    /// A trackpad scroll moving the content by this many points.
    Scroll(Vec2),
    /// A pinch by this factor (above 1 spreads).
    Pinch(f32),
}

/// The sequence, decided one step at a time from the view.
struct Script {
    phase: Phase,
    /// Steps taken in the current phase.
    count: usize,
    /// Steps of the phase before, to undo in this one.
    undo: usize,
}

impl Script {
    /// The next step and its phase, or `None` at the end. `width` is the traces' width.
    fn next(&mut self, view: &ViewState, width: f32) -> Option<(Phase, Input)> {
        let whole = view.span() >= view.duration();
        let pinch = (1.0 / PINCHES_PER_DOUBLING).exp2();
        loop {
            let input = match self.phase {
                Phase::Opening => None,
                Phase::Pan => (self.count < PAN_STEPS).then_some(Input::Key(Action::PanRight)),
                Phase::ZoomOut => (!whole).then_some(Input::Key(Action::ZoomOut)),
                Phase::ZoomIn => (self.count < self.undo).then_some(Input::Key(Action::ZoomIn)),
                Phase::PageDown => (view.visible_rows().end < view.row_count())
                    .then_some(Input::Key(Action::ChannelPageDown)),
                Phase::PageUp => {
                    (self.count < self.undo).then_some(Input::Key(Action::ChannelPageUp))
                }
                Phase::Glide => (self.count < GLIDE_FRAMES)
                    .then_some(Input::Scroll(Vec2::new(-width / 100.0, 0.0))),
                Phase::PinchOut => (!whole).then_some(Input::Pinch(1.0 / pinch)),
                Phase::PinchIn => (self.count < self.undo).then_some(Input::Pinch(pinch)),
                Phase::Done => return None,
            };
            if let Some(input) = input {
                self.count += 1;
                return Some((self.phase, input));
            }
            self.undo = self.count;
            self.count = 0;
            self.phase = self.phase.after();
        }
    }
}

/// Frame intervals and step latencies, in seconds, per phase.
#[derive(Default)]
struct Report {
    frames: Vec<(Phase, f64)>,
    steps: Vec<(Phase, f64)>,
    /// Steps whose view wasn't complete within [`STEP_LIMIT`], and frames of the continuous
    /// phases drawn partly from coarser tiles.
    incomplete: Vec<Phase>,
    opening: Option<f64>,
}

impl Report {
    fn print(&self) {
        match self.opening {
            Some(s) => println!("first complete view after {}", ms(s)),
            None => println!("first view not complete after {} s", OPEN_LIMIT.as_secs()),
        }
        println!(
            "{:<10} {:>6} {:>9} {:>9} {:>9}   {:>6} {:>9} {:>9} {:>9}   {:>10}",
            "", "frames", "p50", "p99", "max", "steps", "p50", "p99", "max", "incomplete"
        );
        let rows = Phase::MEASURED
            .iter()
            .map(|&p| (p.name(), vec![p]))
            .chain([("all", Phase::MEASURED.to_vec())]);
        for (name, phases) in rows {
            let pick = |v: &[(Phase, f64)]| -> Vec<f64> {
                v.iter()
                    .filter(|(p, _)| phases.contains(p))
                    .map(|&(_, s)| s)
                    .collect()
            };
            let frames = Stats::of(pick(&self.frames));
            let steps = Stats::of(pick(&self.steps));
            let incomplete = self
                .incomplete
                .iter()
                .filter(|p| phases.contains(p))
                .count();
            println!(
                "{name:<10} {:>6} {:>9} {:>9} {:>9}   {:>6} {:>9} {:>9} {:>9}   {incomplete:>10}",
                frames.n,
                ms(frames.p50),
                ms(frames.p99),
                ms(frames.max),
                steps.n,
                ms(steps.p50),
                ms(steps.p99),
                ms(steps.max),
            );
        }
        println!(
            "frames: from one frame's start to the next. steps: from the change to a complete \
             view, up to {} s; incomplete: steps that weren't complete by then, and frames of \
             the continuous phases (glide, pinch) drawn partly from coarser tiles.",
            STEP_LIMIT.as_secs()
        );
    }
}

struct Stats {
    n: usize,
    p50: f64,
    p99: f64,
    max: f64,
}

impl Stats {
    /// Nearest-rank percentiles.
    fn of(mut values: Vec<f64>) -> Stats {
        values.sort_by(f64::total_cmp);
        let rank = |p: f64| {
            let i = (p * values.len() as f64).ceil() as usize;
            values.get(i.saturating_sub(1)).copied().unwrap_or(f64::NAN)
        };
        Stats {
            n: values.len(),
            p50: rank(0.5),
            p99: rank(0.99),
            max: values.last().copied().unwrap_or(f64::NAN),
        }
    }
}

fn ms(seconds: f64) -> String {
    if seconds.is_nan() {
        "–".into()
    } else {
        format!("{:.1} ms", seconds * 1e3)
    }
}

/// The window's app: kymo's, with the sequence and the clock around it.
struct Bench {
    app: KymoApp,
    script: Script,
    report: Rc<RefCell<Report>>,
    started: Instant,
    /// When the last frame started, and the phase of the step it drew.
    last_frame: Option<(Instant, Option<Phase>)>,
    /// The current step's phase and when it was taken.
    step: Option<(Phase, Instant)>,
    /// The middle of the traces, where the pointer is kept for scrolls and pinches.
    pointer: Option<Pos2>,
    /// A scroll or pinch for the next frame's input.
    event: Option<Event>,
}

impl Bench {
    fn new(app: KymoApp, report: Rc<RefCell<Report>>) -> Bench {
        Bench {
            app,
            script: Script {
                phase: Phase::Opening,
                count: 0,
                undo: 0,
            },
            report,
            started: Instant::now(),
            last_frame: None,
            step: None,
            pointer: None,
            event: None,
        }
    }

    /// Takes the next step, or closes the window after the last.
    fn advance(&mut self, ctx: &egui::Context) {
        let Some(viewer) = self.app.viewer() else {
            return;
        };
        let Some(layout) = viewer.layout() else {
            return;
        };
        self.pointer = Some(layout.traces.center());
        match self.script.next(viewer.view(), layout.traces.width()) {
            Some((phase, input)) => {
                match input {
                    Input::Key(action) => self.app.perform(ctx, action),
                    Input::Scroll(delta) => {
                        self.event = Some(Event::MouseWheel {
                            unit: MouseWheelUnit::Point,
                            delta,
                            phase: TouchPhase::Move,
                            source: MouseWheelSource::Unknown,
                            modifiers: egui::Modifiers::NONE,
                        });
                    }
                    Input::Pinch(factor) => self.event = Some(Event::Zoom(factor)),
                }
                self.step = Some((phase, Instant::now()));
            }
            None => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }
}

impl eframe::App for Bench {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let ctx = ui.ctx().clone();
        if self.script.phase == Phase::Done {
            // Closing.
            self.app.frame(ui);
            return;
        }
        // The time since the last frame started went to drawing that frame: it counts
        // towards the step it drew (none while opening).
        if let Some((last, Some(phase))) = self.last_frame {
            self.report
                .borrow_mut()
                .frames
                .push((phase, (now - last).as_secs_f64()));
        }
        self.last_frame = Some((now, self.step.map(|(phase, _)| phase)));

        self.app.frame(ui);
        ctx.request_repaint();
        if let Some(error) = self.app.error() {
            panic!("cannot open the recording: {error}");
        }
        let Some(complete) = self.app.viewer().map(|v| v.complete()) else {
            return;
        };
        let end = Instant::now();
        let mut report = self.report.borrow_mut();
        match self.step {
            None => {
                let waited = end - self.started;
                if !complete && waited < OPEN_LIMIT {
                    return;
                }
                report.opening = complete.then_some(waited.as_secs_f64());
            }
            Some((phase, _)) if phase.continuous() => {
                if !complete {
                    report.incomplete.push(phase);
                }
            }
            Some((phase, taken)) => {
                let waited = end - taken;
                if !complete && waited < STEP_LIMIT {
                    return;
                }
                report.steps.push((phase, waited.as_secs_f64()));
                if !complete {
                    report.incomplete.push(phase);
                }
            }
        }
        drop(report);
        self.advance(&ctx);
    }

    /// Keeps the pointer over the traces, whatever the real one does, and adds the step's
    /// scroll or pinch.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(pointer) = self.pointer {
            raw_input
                .events
                .retain(|e| !matches!(e, Event::PointerMoved(_) | Event::PointerGone));
            raw_input.events.push(Event::PointerMoved(pointer));
        }
        raw_input.events.extend(self.event.take());
    }
}

/// The process's peak resident memory so far.
#[cfg(unix)]
#[allow(unsafe_code)]
fn peak_resident_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` fills the struct it is given and reads nothing else.
    let usage = unsafe {
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return None;
        }
        usage.assume_init()
    };
    let max = u64::try_from(usage.ru_maxrss).ok()?;
    // Bytes on macOS, kilobytes elsewhere.
    Some(if cfg!(target_os = "macos") {
        max
    } else {
        max * 1024
    })
}

#[cfg(not(unix))]
fn peak_resident_bytes() -> Option<u64> {
    None
}

/// The process's peak physical footprint so far, as Activity Monitor's Memory column counts
/// it: resident memory without the clean file pages (of the mapped recording and sidecar)
/// that the system can drop at any time.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn peak_footprint_bytes() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: `proc_pid_rusage` fills the struct of the flavour it is given (V4 here) and
    // reads nothing else.
    let info = unsafe {
        let buffer = info.as_mut_ptr().cast::<libc::rusage_info_t>();
        if libc::proc_pid_rusage(std::process::id() as i32, libc::RUSAGE_INFO_V4, buffer) != 0 {
            return None;
        }
        info.assume_init()
    };
    Some(info.ri_lifetime_max_phys_footprint)
}

#[cfg(not(target_os = "macos"))]
fn peak_footprint_bytes() -> Option<u64> {
    None
}
