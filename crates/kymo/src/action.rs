//! What the user can ask for: discrete actions (keys, menu items) and continuous gestures
//! (scrolling, pinching).

use kymo_core::stim::{EventOptions, EventRule};

/// A command from a key or a menu item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    Open,
    Close,
    Quit,
    Settings,
    Shortcuts,
    /// An About window; macOS uses the system's About panel instead.
    About,
    /// A quarter window back or forward in time.
    PanLeft,
    PanRight,
    /// A whole window back or forward.
    PageLeft,
    PageRight,
    ToStart,
    ToEnd,
    ZoomIn,
    ZoomOut,
    /// 20 s from the current start.
    DefaultWindow,
    /// The whole recording.
    Fit,
    ChannelUp,
    ChannelDown,
    ChannelPageUp,
    ChannelPageDown,
    FirstChannels,
    LastChannels,
    MoreChannels,
    FewerChannels,
    /// Taller traces: a smaller amplitude per channel spacing.
    ScaleUp,
    ScaleDown,
    ToggleDc,
    /// Toggles bad for the channel under the pointer.
    ToggleBad,
    /// Shows or hides the sidebar (file header and channel list).
    ToggleSidebar,
    /// Stim events at every change to a non-zero code (see [`EventRule`]).
    EventsEveryChange,
    /// Stim events only at changes from 0.
    EventsFromZero,
    /// Stim events only at changes to a larger code.
    EventsIncreasing,
    /// Whether a code at a stim channel's first sample is an event.
    ToggleInitialEvent,
}

impl Action {
    pub const ALL: [Action; 33] = [
        Action::Open,
        Action::Close,
        Action::Quit,
        Action::Settings,
        Action::Shortcuts,
        Action::About,
        Action::PanLeft,
        Action::PanRight,
        Action::PageLeft,
        Action::PageRight,
        Action::ToStart,
        Action::ToEnd,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::DefaultWindow,
        Action::Fit,
        Action::ChannelUp,
        Action::ChannelDown,
        Action::ChannelPageUp,
        Action::ChannelPageDown,
        Action::FirstChannels,
        Action::LastChannels,
        Action::MoreChannels,
        Action::FewerChannels,
        Action::ScaleUp,
        Action::ScaleDown,
        Action::ToggleDc,
        Action::ToggleBad,
        Action::ToggleSidebar,
        Action::EventsEveryChange,
        Action::EventsFromZero,
        Action::EventsIncreasing,
        Action::ToggleInitialEvent,
    ];

    /// The name in menus and the shortcut overview.
    pub fn label(self) -> &'static str {
        match self {
            Action::Open => "Open…",
            Action::Close => "Close",
            Action::Quit => "Quit",
            Action::Settings => "Settings…",
            Action::Shortcuts => "Keyboard Shortcuts",
            Action::About => "About Kymo",
            Action::PanLeft => "Back a Quarter Window",
            Action::PanRight => "Forward a Quarter Window",
            Action::PageLeft => "Back a Window",
            Action::PageRight => "Forward a Window",
            Action::ToStart => "Start of Recording",
            Action::ToEnd => "End of Recording",
            Action::ZoomIn => "Zoom In",
            Action::ZoomOut => "Zoom Out",
            Action::DefaultWindow => "Default Window",
            Action::Fit => "Fit Recording",
            Action::ChannelUp => "Channel Up",
            Action::ChannelDown => "Channel Down",
            Action::ChannelPageUp => "Page of Channels Up",
            Action::ChannelPageDown => "Page of Channels Down",
            Action::FirstChannels => "First Channels",
            Action::LastChannels => "Last Channels",
            Action::MoreChannels => "More Channels",
            Action::FewerChannels => "Fewer Channels",
            Action::ScaleUp => "Scale Up",
            Action::ScaleDown => "Scale Down",
            Action::ToggleDc => "Remove DC",
            Action::ToggleBad => "Toggle Bad Channel",
            Action::ToggleSidebar => "Show Sidebar",
            Action::EventsEveryChange => "Every Change",
            Action::EventsFromZero => "From Zero",
            Action::EventsIncreasing => "Increasing",
            Action::ToggleInitialEvent => "Initial Event",
        }
    }

    /// Whether menus show the action with a checkmark when it is on.
    pub fn checkable(self) -> bool {
        matches!(
            self,
            Action::ToggleDc
                | Action::ToggleSidebar
                | Action::EventsEveryChange
                | Action::EventsFromZero
                | Action::EventsIncreasing
                | Action::ToggleInitialEvent
        )
    }

    /// The stim event options after this action, if it is one of the stim event items.
    pub fn event_options(self, options: EventOptions) -> Option<EventOptions> {
        let rule = |rule| EventOptions { rule, ..options };
        match self {
            Action::EventsEveryChange => Some(rule(EventRule::EveryChange)),
            Action::EventsFromZero => Some(rule(EventRule::FromZero)),
            Action::EventsIncreasing => Some(rule(EventRule::Increasing)),
            Action::ToggleInitialEvent => Some(EventOptions {
                initial_event: !options.initial_event,
                ..options
            }),
            _ => None,
        }
    }

    /// Whether the action is on under the stim event `options`, if it is one of their items.
    pub fn event_checked(self, options: EventOptions) -> Option<bool> {
        match self {
            Action::EventsEveryChange => Some(options.rule == EventRule::EveryChange),
            Action::EventsFromZero => Some(options.rule == EventRule::FromZero),
            Action::EventsIncreasing => Some(options.rule == EventRule::Increasing),
            Action::ToggleInitialEvent => Some(options.initial_event),
            _ => None,
        }
    }

    /// Whether the action needs an open recording; menu items for it are greyed out
    /// without one.
    pub fn needs_recording(self) -> bool {
        !matches!(
            self,
            Action::Open | Action::Quit | Action::Settings | Action::Shortcuts | Action::About
        )
    }

    pub fn enabled(self, has_recording: bool) -> bool {
        has_recording || !self.needs_recording()
    }
}

/// A continuous input, already in the view's terms. Distances are in points (logical
/// pixels) and follow the content: positive moves it right or down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    /// Moves the traces right by `points`, showing earlier times.
    Pan { points: f64 },
    /// Moves the channel rows down by `points`, showing earlier channels.
    ScrollRows { points: f64 },
    /// Magnifies time by `factor` (above 1 zooms in) around the pointer.
    Zoom { factor: f64 },
    /// Makes traces `factor` times taller.
    Scale { factor: f64 },
}
