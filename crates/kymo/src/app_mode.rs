//! Windows' default app mode (Settings → Personalization → Colors), which the System theme
//! follows there: read from the registry, where that setting keeps it
//! (`AppsUseLightTheme`), and watched for changes.
//!
//! winit and muda ask uxtheme instead (`ShouldAppsUseDarkMode`), which is undocumented and,
//! while Kymo forces a theme on its menus (`native_menu::set_app_mode`), reports the forced
//! one; so egui could keep a forced theme under System. With the mode read here, System
//! becomes Light or Dark before anything is themed, and every part gets the same.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

const UNKNOWN: u8 = 0;
const LIGHT: u8 = 1;
const DARK: u8 = 2;

/// The app mode, shared with the thread that watches it.
#[derive(Clone, Debug, Default)]
pub struct AppMode(Arc<AtomicU8>);

impl AppMode {
    /// Not known yet; [`set`](Self::set) tells it (for tests; [`watch`](Self::watch) on
    /// Windows).
    pub fn new() -> AppMode {
        AppMode::default()
    }

    /// Whether apps should be dark; `None` if unknown.
    pub fn dark(&self) -> Option<bool> {
        match self.0.load(Ordering::Relaxed) {
            LIGHT => Some(false),
            DARK => Some(true),
            _ => None,
        }
    }

    pub fn set(&self, dark: Option<bool>) {
        let value = match dark {
            Some(false) => LIGHT,
            Some(true) => DARK,
            None => UNKNOWN,
        };
        self.0.store(value, Ordering::Relaxed);
    }

    /// The app mode as the registry has it, kept up to date by a thread that waits for the
    /// registry to change and then wakes `ctx`. Unknown where the value can't be read.
    #[cfg(windows)]
    pub fn watch(ctx: &eframe::egui::Context) -> AppMode {
        let mode = AppMode::new();
        let Some(key) = registry::open() else {
            return mode;
        };
        mode.set(registry::apps_dark(&key));
        let (shared, ctx) = (mode.clone(), ctx.clone());
        // Without the thread, the mode is read once and not followed.
        let _ = std::thread::Builder::new()
            .name("kymo app mode".into())
            .spawn(move || {
                // Blocked until a value of the key changes (others too, as transparency);
                // a frame only if the mode did.
                while registry::wait(&key) {
                    let dark = registry::apps_dark(&key);
                    if dark != shared.dark() {
                        shared.set(dark);
                        ctx.request_repaint();
                    }
                }
            });
        mode
    }
}

#[cfg(windows)]
mod registry {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_QUERY_VALUE, REG_NOTIFY_CHANGE_LAST_SET,
        RRF_RT_REG_DWORD, RegCloseKey, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW,
    };

    /// The key holding the Colors settings, open for reading and change notices.
    pub struct Key(HKEY);

    // SAFETY: a registry key handle may be used from any thread.
    #[allow(unsafe_code)]
    unsafe impl Send for Key {}

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle came from `RegOpenKeyExW` and is closed once.
            #[allow(unsafe_code)]
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    pub fn open() -> Option<Key> {
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: a static path and a pointer to a local the call fills.
        #[allow(unsafe_code)]
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                windows_sys::w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                0,
                KEY_NOTIFY | KEY_QUERY_VALUE,
                &mut key,
            )
        };
        (status == ERROR_SUCCESS).then_some(Key(key))
    }

    /// Whether the app mode is dark: `AppsUseLightTheme` 0. `None` without the value
    /// (Windows before 10 1607, where apps are light).
    pub fn apps_dark(key: &Key) -> Option<bool> {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: `key` is open; the call writes at most `size` bytes into `value`.
        #[allow(unsafe_code)]
        let status = unsafe {
            RegGetValueW(
                key.0,
                std::ptr::null(),
                windows_sys::w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut value).cast(),
                &mut size,
            )
        };
        (status == ERROR_SUCCESS).then_some(value == 0)
    }

    /// Waits until a value of `key` changes; `false` if it can't.
    pub fn wait(key: &Key) -> bool {
        // SAFETY: `key` is open; synchronous, so no event handle is needed.
        #[allow(unsafe_code)]
        let status = unsafe {
            RegNotifyChangeKeyValue(
                key.0,
                0,
                REG_NOTIFY_CHANGE_LAST_SET,
                std::ptr::null_mut(),
                0,
            )
        };
        status == ERROR_SUCCESS
    }
}
