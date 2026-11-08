//! The window: no recording, one being opened, one on screen, or why it couldn't open.

use std::path::{Path, PathBuf};

use anyhow::Result;
use eframe::egui::{self, Align, Color32, Event, Layout, RichText, Ui};
use kymo_core::pyramid::Sidecar;
use kymo_core::service::{PyramidStore, ServiceOptions, TileService};
use kymo_core::stim::EventOptions;

use crate::action::Action;
use crate::annotations::AnnotationColors;
use crate::document::{self, Document, Loading, error_text, file_name};
use crate::keymap::{Keymap, Platform};
#[cfg(any(target_os = "macos", windows))]
use crate::native_menu::NativeMenu;
use crate::settings::{Settings, load_annotation_colors, save_annotation_colors};
use crate::ui::gpu::Gpu;
use crate::ui::menus::{MenuState, about_window, add_symbol_font, egui_menu_bar, shortcuts_window};
use crate::ui::settings::settings_window;
use crate::viewer::Viewer;

const APP_NAME: &str = "Kymo";

/// How the window keeps recordings and shows its menus, and the settings it starts with.
#[derive(Clone, Debug)]
pub struct Options {
    /// Where pyramid sidecars go; `None` keeps pyramids in memory.
    pub sidecar_dir: Option<PathBuf>,
    pub menus: Menus,
    pub settings: Settings,
    /// Colors picked for annotation descriptions, as remembered from earlier runs.
    pub annotation_colors: AnnotationColors,
}

/// Where the menu bar comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menus {
    /// The system's, handed over with [`KymoApp::set_native_menu`] (macOS, Windows).
    Native,
    /// Drawn by egui in the window, as on `Platform` (Linux).
    Egui(Platform),
}

impl Default for Options {
    /// Sidecars in the user's cache directory; native menus where the platform has them;
    /// default settings, no annotation colors picked.
    fn default() -> Self {
        let platform = Platform::current();
        Options {
            sidecar_dir: Sidecar::default_dir(),
            menus: if platform.native_menu() {
                Menus::Native
            } else {
                Menus::Egui(platform)
            },
            settings: Settings::default(),
            annotation_colors: AnnotationColors::new(),
        }
    }
}

pub struct KymoApp {
    state: State,
    gpu: Gpu,
    keymap: Keymap,
    options: Options,
    #[cfg(any(target_os = "macos", windows))]
    native_menu: Option<NativeMenu>,
    show_sidebar: bool,
    /// How stim codes are annotated, for this recording and the ones opened after it.
    stim_options: EventOptions,
    shortcuts_open: bool,
    about_open: bool,
    settings_open: bool,
    /// Show the file dialog once the window is on screen.
    pick_file: bool,
    /// Whether a frame has been shown yet.
    shown: bool,
}

/// A recording ready to show, as the loading thread hands it over.
type Opened = (Document, TileService);

enum State {
    Empty,
    Loading(Loading<Opened>),
    Ready(Box<Viewer>),
    Failed { path: PathBuf, error: String },
}

impl KymoApp {
    /// A window opening `path`, or showing a file dialog without one.
    pub fn new(ctx: &egui::Context, gpu: Gpu, options: Options, path: Option<PathBuf>) -> Self {
        // ⌘+ and ⌘− zoom time, not the interface.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        add_symbol_font(ctx);
        gpu.set_budget(options.settings.gpu_memory_bytes());
        let mut app = KymoApp {
            state: State::Empty,
            gpu,
            keymap: Keymap::new(Platform::current()),
            options,
            #[cfg(any(target_os = "macos", windows))]
            native_menu: None,
            show_sidebar: true,
            stim_options: EventOptions::default(),
            shortcuts_open: false,
            about_open: false,
            settings_open: false,
            pick_file: path.is_none(),
            shown: false,
        };
        if let Some(path) = path {
            app.open(ctx, path);
        }
        app
    }

    /// Takes over the native menu bar, already installed: its items act from now on, and
    /// its shortcuts no longer reach egui. Without one, [`Menus::Native`] draws the menu bar
    /// with egui.
    #[cfg(any(target_os = "macos", windows))]
    pub fn set_native_menu(&mut self, menu: NativeMenu) {
        self.native_menu = Some(menu);
    }

    /// Whether the native menu bar takes its shortcuts before egui sees them.
    fn menu_takes_keys(&self) -> bool {
        #[cfg(any(target_os = "macos", windows))]
        if self.native_menu.is_some() {
            return self.keymap.platform().menu_takes_shortcuts();
        }
        false
    }

    /// The platform whose menu bar egui draws, if egui draws one.
    fn egui_menus(&self) -> Option<Platform> {
        match self.options.menus {
            Menus::Egui(platform) => Some(platform),
            #[cfg(any(target_os = "macos", windows))]
            Menus::Native if self.native_menu.is_some() => None,
            Menus::Native => Some(self.keymap.platform()),
        }
    }

    /// Whether the sidebar is shown.
    pub fn sidebar_shown(&self) -> bool {
        self.show_sidebar
    }

    /// Whether the shortcut overview is open.
    pub fn shortcuts_open(&self) -> bool {
        self.shortcuts_open
    }

    /// Whether the Settings window is open.
    pub fn settings_open(&self) -> bool {
        self.settings_open
    }

    pub fn settings(&self) -> &Settings {
        &self.options.settings
    }

    /// The GPU the traces are drawn on.
    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// Changes the settings and applies them at once: the tile cache and GPU budgets (a
    /// smaller one evicts), anti-aliasing, and the default window for ⌘0 and recordings
    /// opened from now on.
    pub fn set_settings(&mut self, settings: Settings) {
        let settings = settings.clamped();
        self.options.settings = settings;
        self.gpu.set_budget(settings.gpu_memory_bytes());
        if let State::Ready(viewer) = &mut self.state {
            viewer.cache().set_budget(settings.tile_cache_bytes());
            viewer.set_default_span(settings.default_window_s);
            viewer.set_antialias(settings.antialias);
            viewer.set_type_colors(settings.type_colors);
        }
    }

    /// The colors picked for annotation descriptions.
    pub fn annotation_colors(&self) -> &AnnotationColors {
        &self.options.annotation_colors
    }

    /// Whether the About window is open.
    pub fn about_open(&self) -> bool {
        self.about_open
    }

    fn menu_state(&self, ctx: &egui::Context) -> AppMenuState {
        let viewer = self.viewer();
        AppMenuState {
            ready: matches!(self.state, State::Ready(_)),
            typing: ctx.egui_wants_keyboard_input(),
            editing: viewer.is_some_and(Viewer::editing),
            selected: viewer.is_some_and(|v| v.selected().is_some()),
            dragging: viewer.is_some_and(Viewer::dragging),
            can_undo: viewer.is_some_and(|v| v.history().can_undo()),
            can_redo: viewer.is_some_and(|v| v.history().can_redo()),
            sidebar: self.show_sidebar,
            antialias: self.options.settings.antialias,
            channel: self.viewer().is_some_and(|v| v.hover().channel.is_some()),
            dc: self.viewer().is_some_and(|v| v.view().dc_removed()),
            stim: self.viewer().is_some_and(Viewer::has_stim),
            stim_options: self.stim_options,
        }
    }

    /// The recording on screen, if there is one.
    pub fn viewer(&self) -> Option<&Viewer> {
        match &self.state {
            State::Ready(viewer) => Some(viewer),
            _ => None,
        }
    }

    /// Why the last recording couldn't be opened.
    pub fn error(&self) -> Option<&str> {
        match &self.state {
            State::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

    /// Opens `path` on a background thread, with its tile service.
    pub fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        set_title(ctx, Some(&file_name(&path)));
        let store = match &self.options.sidecar_dir {
            Some(dir) => PyramidStore::Sidecar {
                dir: dir.clone(),
                source: path.clone(),
            },
            None => PyramidStore::Memory,
        };
        let repaint = ctx.clone();
        let tiles = ctx.clone();
        let mut service_options = ServiceOptions::new(store);
        service_options.cache_budget = self.options.settings.tile_cache_bytes();
        let open = move |path: &Path| -> Result<Opened> {
            let document = document::open(path)?;
            let service =
                TileService::new(document.recording.clone(), service_options, move || {
                    tiles.request_repaint()
                })?;
            Ok((document, service))
        };
        self.state = State::Loading(Loading::start(path, open, move || {
            repaint.request_repaint();
        }));
    }

    /// One frame: picks up a finished open, handles keys, and draws the window.
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.poll_loading(&ctx);
        // The startup dialog is modal: show it once the window has been drawn and has focus.
        // On macOS a panel opened before the app is active is canceled at once; gaining
        // focus repaints, which brings us back here.
        if self.pick_file {
            let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
            if self.shown && focused {
                self.pick_file = false;
                self.pick_and_open(&ctx);
            } else if !self.shown {
                ctx.request_repaint();
            }
        }
        self.shown = true;

        #[cfg(any(target_os = "macos", windows))]
        let mut actions = self
            .native_menu
            .as_ref()
            .map(NativeMenu::poll)
            .unwrap_or_default();
        #[cfg(not(any(target_os = "macos", windows)))]
        let mut actions = Vec::new();
        actions.extend(self.key_actions(&ctx));
        if let Some(platform) = self.egui_menus() {
            let state = self.menu_state(&ctx);
            egui::Panel::top("menu bar").show(ui, |ui| {
                actions.extend(egui_menu_bar(ui, platform, &state));
            });
        }
        for action in actions {
            self.perform(&ctx, action);
        }
        shortcuts_window(&ctx, &mut self.shortcuts_open, self.keymap.platform());
        about_window(&ctx, &mut self.about_open);
        let mut settings = self.options.settings;
        if settings_window(&ctx, &mut self.settings_open, &mut settings) {
            self.set_settings(settings);
        }
        #[cfg(any(target_os = "macos", windows))]
        {
            let state = self.menu_state(&ctx);
            if let Some(menu) = &mut self.native_menu {
                menu.update(&state);
            }
        }

        let mut open_clicked = false;
        match &mut self.state {
            State::Ready(viewer) => {
                open_clicked = viewer.show(
                    ui,
                    &self.gpu,
                    &self.keymap,
                    self.show_sidebar,
                    &mut self.options.annotation_colors,
                );
            }
            State::Empty => {
                egui::CentralPanel::default().show(ui, |ui| {
                    open_clicked = centered_message(ui, "No recording open.", None);
                });
            }
            State::Loading(loading) => {
                let text = format!("Opening {}…", loading.path().display());
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.horizontal_centered(|ui| {
                            ui.spinner();
                            ui.label(text);
                        });
                    });
                });
            }
            State::Failed { error, .. } => {
                let color = ui.visuals().error_fg_color;
                egui::CentralPanel::default().show(ui, |ui| {
                    open_clicked =
                        centered_message(ui, "Could not open the recording.", Some((error, color)));
                });
            }
        }
        if open_clicked {
            self.pick_and_open(&ctx);
        }
    }

    /// Carries out `action`, unless it is disabled now.
    pub fn perform(&mut self, ctx: &egui::Context, action: Action) {
        let state = self.menu_state(ctx);
        if !state.enabled(action) {
            return;
        }
        match action {
            // A text field with the keyboard undoes its own typing; on macOS the menu takes
            // ⌘Z before the field sees it, so it is handed on.
            Action::Undo | Action::Redo if state.typing => {
                if let Some(event) = text_field_key(action) {
                    ctx.input_mut(|i| i.events.push(event));
                }
            }
            Action::Open => self.pick_and_open(ctx),
            Action::Close => {
                self.state = State::Empty;
                set_title(ctx, None);
            }
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::Shortcuts => self.shortcuts_open = !self.shortcuts_open,
            Action::About => self.about_open = true,
            Action::ToggleSidebar => self.show_sidebar = !self.show_sidebar,
            Action::ToggleAntialias => self.set_settings(Settings {
                antialias: !self.options.settings.antialias,
                ..self.options.settings
            }),
            Action::Settings => self.settings_open = true,
            _ => {
                if let Some(options) = action.stim_options(self.stim_options) {
                    self.stim_options = options;
                    if let State::Ready(viewer) = &mut self.state {
                        viewer.set_stim_options(options);
                    }
                } else if let State::Ready(viewer) = &mut self.state {
                    viewer.apply(action);
                }
            }
        }
    }

    /// The actions of this frame's key presses, unless a text field has the keyboard. Esc
    /// with a menu or popup open only closes it.
    fn key_actions(&self, ctx: &egui::Context) -> Vec<Action> {
        if ctx.egui_wants_keyboard_input() {
            return Vec::new();
        }
        let popup = egui::Popup::is_any_open(ctx);
        let actions: Vec<Action> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|event| match *event {
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } if self.menu_takes_keys() => self.keymap.egui_action(key, modifiers),
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => self.keymap.action(key, modifiers),
                    _ => None,
                })
                .collect()
        });
        actions
            .into_iter()
            .filter(|&a| !(popup && a == Action::Deselect))
            .collect()
    }

    fn pick_and_open(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Open a recording")
            .add_filter("EDF and BDF", &["edf", "bdf", "EDF", "BDF"])
            .add_filter("All files", &["*"]);
        if let Some(dir) = self.current_path().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open(ctx, path);
        }
    }

    fn current_path(&self) -> Option<&Path> {
        match &self.state {
            State::Empty => None,
            State::Loading(loading) => Some(loading.path()),
            State::Ready(viewer) => Some(&viewer.document().path),
            State::Failed { path, .. } => Some(path),
        }
    }

    fn poll_loading(&mut self, ctx: &egui::Context) {
        let State::Loading(loading) = &self.state else {
            return;
        };
        let Some(result) = loading.poll() else {
            return;
        };
        self.state = match result {
            Ok((document, service)) => {
                set_title(ctx, Some(&document.name()));
                let mut viewer = Viewer::new(
                    document,
                    service,
                    self.options.settings.default_window_s,
                    self.stim_options,
                );
                viewer.set_antialias(self.options.settings.antialias);
                viewer.set_type_colors(self.options.settings.type_colors);
                if self.options.settings.auto_scale_on_open {
                    viewer.apply(Action::AutoScale);
                }
                State::Ready(Box::new(viewer))
            }
            Err(error) => State::Failed {
                path: loading.path().to_owned(),
                error: error_text(&error),
            },
        };
    }
}

impl eframe::App for KymoApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    /// Keeps the settings and the annotation colors for the next run.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.options.settings.save(storage);
        save_annotation_colors(&self.options.annotation_colors, storage);
    }
}

/// What the menus show: whether a recording is open, what it has, and the toggles' states.
struct AppMenuState {
    ready: bool,
    /// Whether a text field has the keyboard, for Undo and Redo to act on.
    typing: bool,
    /// Whether annotation mode is on, and an annotation selected.
    editing: bool,
    selected: bool,
    /// Whether a drag is changing an annotation: until it ends, nothing else may, and Esc
    /// takes it back.
    dragging: bool,
    can_undo: bool,
    can_redo: bool,
    sidebar: bool,
    antialias: bool,
    /// Whether the pointer is over a channel, which Toggle Bad acts on.
    channel: bool,
    dc: bool,
    /// Whether the recording has stim channels, whose codes the rule annotates.
    stim: bool,
    stim_options: EventOptions,
}

impl MenuState for AppMenuState {
    fn enabled(&self, action: Action) -> bool {
        // Toggle Bad needs a channel to act on.
        let now = match action {
            Action::ToggleBad => self.channel,
            Action::Undo => self.can_undo && !self.dragging || self.typing,
            Action::Redo => self.can_redo && !self.dragging || self.typing,
            Action::DeleteAnnotation => self.selected && !self.dragging,
            Action::Deselect => self.selected || self.dragging,
            _ if action.stim_checked(self.stim_options).is_some() => self.stim,
            _ => true,
        };
        action.enabled(self.ready) && now
    }

    fn checked(&self, action: Action) -> Option<bool> {
        match action {
            Action::ToggleSidebar => Some(self.sidebar),
            Action::ToggleAntialias => Some(self.antialias),
            Action::ToggleDc => Some(self.dc),
            Action::ToggleEditing => Some(self.editing),
            _ => action.stim_checked(self.stim_options),
        }
    }
}

/// The key press a text field takes as `action`, for Undo and Redo: ⌘Z and ⇧⌘Z.
pub fn text_field_key(action: Action) -> Option<Event> {
    let shift = match action {
        Action::Undo => false,
        Action::Redo => true,
        _ => return None,
    };
    let mut modifiers = egui::Modifiers::COMMAND;
    modifiers.shift = shift;
    if cfg!(target_os = "macos") {
        modifiers.mac_cmd = true;
    } else {
        modifiers.ctrl = true;
    }
    Some(Event::Key {
        key: egui::Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    })
}

fn set_title(ctx: &egui::Context, file: Option<&str>) {
    let title = match file {
        Some(file) => format!("{file} – {APP_NAME}"),
        None => APP_NAME.to_owned(),
    };
    ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
}

/// A message with an optional detail and an Open button, centered. Returns whether the
/// button was clicked.
fn centered_message(ui: &mut Ui, message: &str, detail: Option<(&String, Color32)>) -> bool {
    let mut clicked = false;
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        ui.add_space(ui.available_height() / 3.0);
        ui.heading(message);
        if let Some((detail, color)) = detail {
            ui.add_space(8.0);
            ui.label(RichText::new(detail).color(color));
        }
        ui.add_space(12.0);
        clicked = ui.button("Open…").clicked();
    });
    clicked
}

/// Builds the native menu bar, puts it in place and hands it to `app`, once the window
/// exists. If that fails, egui draws the menu bar.
#[cfg(any(target_os = "macos", windows))]
fn attach_native_menu(app: &mut KymoApp, cc: &eframe::CreationContext<'_>) {
    let install = || -> Result<NativeMenu> {
        let menu = NativeMenu::new(Platform::current(), &cc.egui_ctx)?;
        #[cfg(target_os = "macos")]
        menu.install();
        #[cfg(windows)]
        menu.install(cc)?;
        Ok(menu)
    };
    match install() {
        Ok(menu) => app.set_native_menu(menu),
        Err(error) => eprintln!("kymo: no native menu bar: {error}"),
    }
}

/// No native menu bar on this platform: egui draws one.
#[cfg(not(any(target_os = "macos", windows)))]
fn attach_native_menu(_: &mut KymoApp, _: &eframe::CreationContext<'_>) {}

/// Starts the window and runs until it closes.
pub fn run(path: Option<PathBuf>) -> Result<()> {
    // macOS titles the app menu with the process name, which outside an app bundle is the
    // executable's ("kymo").
    #[cfg(target_os = "macos")]
    objc2_foundation::NSProcessInfo::processInfo()
        .setProcessName(objc2_foundation::ns_string!(APP_NAME));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_app_id("kymo")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([480.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| {
            let gpu = Gpu::new(cc.wgpu_render_state.as_ref());
            let options = Options {
                settings: Settings::load(cc.storage),
                annotation_colors: load_annotation_colors(cc.storage),
                ..Options::default()
            };
            let native = options.menus == Menus::Native;
            let mut app = KymoApp::new(&cc.egui_ctx, gpu, options, path);
            if native {
                attach_native_menu(&mut app, cc);
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("cannot open a window: {e}"))
}
