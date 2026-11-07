//! What the user can ask for: discrete actions (keys, menu items) and continuous gestures
//! (scrolling, pinching).

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
}

impl Action {
    pub const ALL: [Action; 28] = [
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
