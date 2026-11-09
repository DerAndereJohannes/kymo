//! The window: no recording, one being opened, one on screen, or why it couldn't open.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use anyhow::Result;
use eframe::egui::{self, Align, Color32, Event, Layout, RichText, Ui};
use kymo_core::annotation_file::{self, Format};
use kymo_core::pyramid::Sidecar;
use kymo_core::service::{PyramidStore, ServiceOptions, TileService};
use kymo_core::stim::EventOptions;

use crate::action::Action;
use crate::annotations::AnnotationColors;
use crate::app_mode::AppMode;
use crate::document::{self, Document, Loading, error_text, file_name};
use crate::keymap::{Keymap, Platform};
#[cfg(any(target_os = "macos", windows))]
use crate::native_menu::NativeMenu;
use crate::settings::{Settings, Theme, load_annotation_colors, save_annotation_colors};
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
    /// The window title last set.
    title: String,
    /// Answers to the next file dialogs, in order, instead of the system's (`None` cancels),
    /// for tests.
    dialog_answers: Option<VecDeque<Option<PathBuf>>>,
    /// What waits for the answer to "Save the changes?".
    leaving: Option<Leave>,
    /// A message in a window: its title and text.
    notice: Option<(String, String)>,
    /// The window may close: its unsaved changes were saved or given up.
    closing: bool,
    /// The system's app mode where winit can't tell it (Windows, Linux), which System
    /// follows there (see [`AppMode`]).
    app_mode: Option<AppMode>,
    /// The theme setting, the system's theme and the app mode it was last applied for.
    theme_applied: Option<(Theme, Option<egui::Theme>, Option<bool>)>,
}

/// What the unsaved changes question holds up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leave {
    Close,
    Open,
    Quit,
}

/// A file dialog.
enum Dialog {
    Open,
    /// Where to save the annotations, suggesting a path.
    Save(PathBuf),
    /// Where to export the shown annotations, suggesting a path.
    Export(PathBuf),
    Load,
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
        // ⌘+ and ⌘− zoom time, not the interface. The title bar's theme is ours: egui sets
        // it only when its theme changes, not again when the system's does (see
        // `apply_theme`).
        ctx.options_mut(|o| {
            o.zoom_with_keyboard = false;
            o.sync_window_theme = false;
        });
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
            // As the window opens.
            title: APP_NAME.to_owned(),
            dialog_answers: None,
            leaving: None,
            notice: None,
            closing: false,
            app_mode: None,
            theme_applied: None,
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
    /// opened from now on; the theme from the next frame.
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

    /// Has System follow `mode` (the system's app mode, read on Windows and Linux) instead of the system's theme
    /// as winit reports it.
    pub fn follow_app_mode(&mut self, mode: AppMode) {
        self.app_mode = Some(mode);
    }

    /// Applies the theme setting to egui, the title bar and the native menus, when it, the
    /// system's theme or the app mode changed. Again on a change of the system's: Windows
    /// puts the title bar back to the system's mode then, whatever the window was set to.
    /// Following an app mode, System is applied as Light or Dark.
    fn apply_theme(&mut self, ctx: &egui::Context) {
        let theme = self.options.settings.theme;
        let app_dark = self.app_mode.as_ref().and_then(AppMode::dark);
        let now = (theme, ctx.system_theme(), app_dark);
        if self.theme_applied == Some(now) {
            return;
        }
        self.theme_applied = Some(now);
        let shown = match (theme, app_dark) {
            (Theme::System, Some(true)) => Theme::Dark,
            (Theme::System, Some(false)) => Theme::Light,
            (theme, _) => theme,
        };
        ctx.set_theme(shown.preference());
        ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(shown.window()));
        #[cfg(any(target_os = "macos", windows))]
        if let Some(menu) = &self.native_menu {
            menu.set_theme(shown);
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
            shown: viewer.is_some_and(|v| v.legend().shown_count().0 > 0),
            // Not looked for during a drag, which changes the store every frame.
            mergeable: viewer.is_some_and(|v| !v.dragging() && v.can_merge()),
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

    /// The window title: the recording's file name, with "(edited)" while its annotations
    /// differ from the opened ones, and the app's name.
    pub fn title(&self) -> String {
        let Some(path) = self.current_path() else {
            return APP_NAME.to_owned();
        };
        let edited = if self.viewer().is_some_and(Viewer::edited) {
            " (edited)"
        } else {
            ""
        };
        format!("{}{edited} – {APP_NAME}", file_name(path))
    }

    /// The title last sent to the window, which [`title`](Self::title) gives at the end of
    /// each frame.
    pub fn window_title(&self) -> &str {
        &self.title
    }

    /// Opens `path` on a background thread, with its tile service.
    pub fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
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
        self.apply_theme(&ctx);
        self.poll_loading();
        // The window's close button, or Quit's own request: unsaved changes are asked about.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.closing
            && self.viewer().is_some_and(Viewer::edited)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.leaving = Some(Leave::Quit);
        }
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
            self.apply_theme(&ctx);
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
            self.leave(&ctx, Leave::Open);
        }
        self.unsaved_question(&ctx);
        self.notice_window(&ctx);
        let title = self.title();
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
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
            Action::Open => self.leave(ctx, Leave::Open),
            Action::Close => self.leave(ctx, Leave::Close),
            Action::SaveAnnotations => {
                self.save_annotations();
            }
            Action::ExportShownAnnotations => {
                if let Some(path) = self.viewer().map(|v| export_path(&v.save_path()))
                    && let Some(path) = self.ask(Dialog::Export(path))
                {
                    self.export_shown_to(&path);
                }
            }
            Action::LoadAnnotations => {
                if let Some(path) = self.ask(Dialog::Load) {
                    self.load_annotations_from(&path);
                }
            }
            Action::Quit => self.leave(ctx, Leave::Quit),
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
        // A question or a notice holds the window until it is answered.
        if ctx.egui_wants_keyboard_input() || self.leaving.is_some() || self.notice.is_some() {
            return Vec::new();
        }
        // Esc in a text field: egui took the keyboard back before this frame's widgets.
        let popup = egui::Popup::is_any_open(ctx) || self.viewer().is_some_and(Viewer::was_typing);
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
        if let Some(path) = self.ask(Dialog::Open) {
            self.open(ctx, path);
        }
    }

    /// Answers the next file dialogs with `answers`, in order, instead of showing the
    /// system's; `None` cancels one. For tests.
    pub fn answer_file_dialogs(&mut self, answers: impl IntoIterator<Item = Option<PathBuf>>) {
        self.dialog_answers = Some(answers.into_iter().collect());
    }

    /// The path a file dialog picks, or `None` if it was canceled.
    fn ask(&mut self, dialog: Dialog) -> Option<PathBuf> {
        if let Some(answers) = &mut self.dialog_answers {
            return answers.pop_front().flatten();
        }
        let mut files = rfd::FileDialog::new();
        if let Some(dir) = self.current_path().and_then(Path::parent) {
            files = files.set_directory(dir);
        }
        let save = |files: rfd::FileDialog, title: &str, suggested: &Path| {
            let mut files = files.set_title(title).add_filter("Events table", &["tsv"]);
            if let Some(dir) = suggested.parent() {
                files = files.set_directory(dir);
            }
            if let Some(name) = suggested.file_name() {
                files = files.set_file_name(name.to_string_lossy());
            }
            files.save_file()
        };
        match dialog {
            Dialog::Open => files
                .set_title("Open a recording")
                .add_filter("EDF and BDF", &["edf", "bdf", "EDF", "BDF"])
                .add_filter("All files", &["*"])
                .pick_file(),
            Dialog::Save(suggested) => save(files, "Save annotations", &suggested),
            Dialog::Export(suggested) => save(files, "Export shown annotations", &suggested),
            Dialog::Load => files
                .set_title("Load annotations")
                .add_filter("Events table or MNE CSV", &["tsv", "csv", "TSV", "CSV"])
                .pick_file(),
        }
    }

    /// Closes the recording, opens another, or quits, after asking what to do with unsaved
    /// changes to the annotations.
    fn leave(&mut self, ctx: &egui::Context, leave: Leave) {
        if self.viewer().is_some_and(Viewer::edited) && !self.closing {
            self.leaving = Some(leave);
        } else {
            self.go(ctx, leave);
        }
    }

    /// Closes the recording, opens another, or quits, without asking.
    fn go(&mut self, ctx: &egui::Context, leave: Leave) {
        match leave {
            Leave::Close => self.state = State::Empty,
            Leave::Open => self.pick_and_open(ctx),
            Leave::Quit => {
                self.closing = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// What waits for the answer to "Save the changes?", while it is asked.
    pub fn leaving(&self) -> Option<Leave> {
        self.leaving
    }

    /// The message shown in a window, if there is one: its title and text.
    pub fn notice(&self) -> Option<(&str, &str)> {
        self.notice.as_ref().map(|(t, s)| (t.as_str(), s.as_str()))
    }

    /// Whether the window may close now.
    pub fn closing(&self) -> bool {
        self.closing
    }

    /// Asks "Save the changes?" while something waits for the answer: Save… saves (and
    /// goes on unless the save dialog was canceled), Don't Save goes on, Cancel stays.
    fn unsaved_question(&mut self, ctx: &egui::Context) {
        let Some(leave) = self.leaving else {
            return;
        };
        let name = self
            .viewer()
            .map_or_else(String::new, |v| v.document().name());
        #[derive(PartialEq)]
        enum Answer {
            Save,
            DontSave,
            Cancel,
        }
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::unique("kymo unsaved changes")).show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.heading(format!("Save the changes to the annotations of {name}?"));
            ui.label("They are lost if you don't save them.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save…").clicked() {
                    answer = Some(Answer::Save);
                }
                if ui.button("Don't Save").clicked() {
                    answer = Some(Answer::DontSave);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(Answer::Cancel);
                }
            });
        });
        if answer.is_none() && modal.should_close() {
            answer = Some(Answer::Cancel);
        }
        let Some(answer) = answer else {
            return;
        };
        self.leaving = None;
        let go_on = match answer {
            Answer::Save => self.save_annotations(),
            Answer::DontSave => true,
            Answer::Cancel => false,
        };
        if go_on {
            self.go(ctx, leave);
        }
    }

    /// Shows the notice, if there is one, until it is dismissed.
    fn notice_window(&mut self, ctx: &egui::Context) {
        let Some((title, text)) = &self.notice else {
            return;
        };
        let mut ok = false;
        let modal = egui::Modal::new(egui::Id::unique("kymo notice")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading(title.as_str());
            ui.label(text.as_str());
            ui.add_space(8.0);
            ok = ui.button("OK").clicked();
        });
        if ok || modal.should_close() {
            self.notice = None;
        }
    }

    /// Asks where to save the annotations and saves them there; whether it did.
    fn save_annotations(&mut self) -> bool {
        let Some(suggested) = self.viewer().map(Viewer::save_path) else {
            return false;
        };
        let Some(path) = self.ask(Dialog::Save(suggested)) else {
            return false;
        };
        self.save_annotations_to(&path)
    }

    /// Saves the file's and the viewer's annotations to `path`, as an events table with
    /// the extension `.tsv` (so nothing reads it as another format, and the recording is
    /// never overwritten); whether it did. An error shows in a notice.
    pub fn save_annotations_to(&mut self, path: &Path) -> bool {
        let path = with_tsv(path);
        let State::Ready(viewer) = &mut self.state else {
            return false;
        };
        match write_file(&path, |out| viewer.write_annotations(out, false)) {
            Ok(()) => {
                viewer.mark_saved(path);
                true
            }
            Err(error) => {
                self.notice = Some((
                    "Could not save the annotations".into(),
                    format!("{}: {error}", path.display()),
                ));
                false
            }
        }
    }

    /// Exports the annotations of the descriptions shown to `path`, as
    /// [`save_annotations_to`](Self::save_annotations_to) does, without marking them saved.
    pub fn export_shown_to(&mut self, path: &Path) -> bool {
        let path = with_tsv(path);
        let Some(viewer) = self.viewer() else {
            return false;
        };
        match write_file(&path, |out| viewer.write_annotations(out, true)) {
            Ok(()) => true,
            Err(error) => {
                self.notice = Some((
                    "Could not export the annotations".into(),
                    format!("{}: {error}", path.display()),
                ));
                false
            }
        }
    }

    /// Adds the annotations of the file at `path` (an events table or MNE's CSV) as one
    /// step; whether it did. A notice tells of an error, and of annotations skipped.
    pub fn load_annotations_from(&mut self, path: &Path) -> bool {
        let State::Ready(viewer) = &mut self.state else {
            return false;
        };
        let name = file_name(path);
        let fail = |text: String| Some(("Could not load the annotations".to_owned(), text));
        let Some(format) = Format::of(path) else {
            self.notice = fail(format!("{name} is not a .tsv or .csv file."));
            return false;
        };
        let context = annotation_file::Context::of(&viewer.document().recording);
        let read = File::open(path)
            .map_err(annotation_file::FileError::from)
            .and_then(|f| annotation_file::read(BufReader::new(f), format, &context));
        let loaded = match read {
            Ok(loaded) => loaded,
            Err(error) => {
                self.notice = fail(format!("{name}: {error}"));
                return false;
            }
        };
        let count = loaded.annotations.len();
        viewer.add_annotations(loaded.annotations);
        let mut skipped = Vec::new();
        if loaded.outside > 0 {
            skipped.push(format!("{} outside the recording", loaded.outside));
        }
        if loaded.unknown_channels > 0 {
            skipped.push(format!(
                "{} naming channels it doesn't have",
                loaded.unknown_channels
            ));
        }
        if !skipped.is_empty() || count == 0 {
            let mut text = format!("Loaded {count} annotation{} from {name}.", plural(count));
            if !skipped.is_empty() {
                text += &format!(" Skipped {}.", skipped.join(" and "));
            }
            self.notice = Some(("Annotations loaded".into(), text));
        }
        true
    }

    fn current_path(&self) -> Option<&Path> {
        match &self.state {
            State::Empty => None,
            State::Loading(loading) => Some(loading.path()),
            State::Ready(viewer) => Some(&viewer.document().path),
            State::Failed { path, .. } => Some(path),
        }
    }

    fn poll_loading(&mut self) {
        let State::Loading(loading) = &self.state else {
            return;
        };
        let Some(result) = loading.poll() else {
            return;
        };
        self.state = match result {
            Ok((document, service)) => {
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
    /// Whether any annotations are shown, for Export Shown Annotations to write.
    shown: bool,
    /// Whether Merge Annotations has anything to merge.
    mergeable: bool,
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
            Action::MergeAnnotations => self.mergeable && !self.dragging,
            Action::Deselect => self.selected || self.dragging,
            Action::ExportShownAnnotations => self.shown,
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

/// `path` with the extension `.tsv`, unless it has it (in any case).
fn with_tsv(path: &Path) -> PathBuf {
    if Format::of(path) == Some(Format::Table) {
        path.to_owned()
    } else {
        let mut name = path.as_os_str().to_owned();
        name.push(".tsv");
        PathBuf::from(name)
    }
}

/// Where to suggest exporting the shown annotations, next to `saved`.
fn export_path(saved: &Path) -> PathBuf {
    let stem = saved
        .file_stem()
        .map_or("annotations".into(), |s| s.to_string_lossy());
    saved.with_file_name(format!("{stem}-shown.tsv"))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Writes a file with `write`, then flushes it.
fn write_file(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<File>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    write(&mut out)?;
    out.flush()
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
        let menu = {
            let mut menu = menu;
            menu.install(cc)?;
            menu
        };
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
            #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
            app.follow_app_mode(AppMode::watch(&cc.egui_ctx));
            if native {
                attach_native_menu(&mut app, cc);
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("cannot open a window: {e}"))
}
