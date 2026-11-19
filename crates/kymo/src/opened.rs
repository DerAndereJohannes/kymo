//! Files the system hands the running app, as opposed to its command line: on macOS, Finder
//! sends an app bundle the files to open (Open With, a double-click) as an Apple event, at
//! launch and while it runs, never as arguments. Windows and Linux pass them as arguments to
//! a new process, so they need none of this.
//!
//! The [`Inbox`] keeps the paths until the window takes them, as it does a dropped file.

use std::path::PathBuf;
use std::sync::Mutex;

use eframe::egui;

/// Paths the system asked the app to open, kept until the window takes them.
#[derive(Debug, Default)]
pub struct Inbox {
    paths: Mutex<Vec<PathBuf>>,
    /// The window's context, to wake it when paths arrive; none before it opens.
    ctx: Mutex<Option<egui::Context>>,
}

impl Inbox {
    pub fn new() -> Inbox {
        Inbox::default()
    }

    /// Keeps `paths` for the window and wakes it.
    pub fn push(&self, paths: impl IntoIterator<Item = PathBuf>) {
        lock(&self.paths).extend(paths);
        if let Some(ctx) = lock(&self.ctx).as_ref() {
            ctx.request_repaint();
        }
    }

    /// The paths kept since the last call.
    pub fn take(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *lock(&self.paths))
    }

    /// Whether paths wait to be taken.
    pub fn is_empty(&self) -> bool {
        lock(&self.paths).is_empty()
    }

    /// Wakes the window with `ctx` whenever paths arrive from now on.
    pub fn wake(&self, ctx: &egui::Context) {
        *lock(&self.ctx) = Some(ctx.clone());
    }
}

/// The lock, even if a thread panicked while holding it (the data stays consistent: each
/// use is a single push, take or replace).
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(target_os = "macos")]
pub use macos::{add_open_urls, receive_on};

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::CStr;
    use std::path::PathBuf;
    use std::sync::{Arc, OnceLock};

    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::sel;
    use objc2_foundation::{NSArray, NSURL};

    use super::Inbox;

    /// Where `application:openURLs:` puts its paths, set once by [`receive_on`].
    static INBOX: OnceLock<Arc<Inbox>> = OnceLock::new();

    /// Makes `application:openURLs:` put its paths in `inbox`. Only the first call counts
    /// (there is one app).
    pub fn receive_on(inbox: Arc<Inbox>) {
        let _ = INBOX.set(inbox);
    }

    /// Adds `application:openURLs:` to `class`, the app delegate's: winit registers its own
    /// delegate (which starts its event loop when the app has launched) and leaves that method
    /// out, so AppKit would answer a file from Finder with "cannot open files in this format".
    /// Call it after winit's event loop is created and before it runs, as the files a launch
    /// was started with arrive before the app has finished launching. False if the class has
    /// the method already (it is left as it is).
    #[allow(unsafe_code)]
    pub fn add_open_urls(class: &AnyClass) -> bool {
        let types: &CStr = c"v@:@@";
        // SAFETY: `open_urls` has the signature `types` encodes (no return value; self, the
        // selector, two objects), which is that of `application:openURLs:`, and AppKit calls
        // it only on the main thread, with an `NSApplication` and an `NSArray<NSURL>`.
        // `class_addMethod` copies the type string and doesn't replace an existing method.
        unsafe {
            let imp: Imp = std::mem::transmute::<
                unsafe extern "C-unwind" fn(
                    *mut AnyObject,
                    Sel,
                    *mut AnyObject,
                    *mut NSArray<NSURL>,
                ),
                Imp,
            >(open_urls);
            objc2::ffi::class_addMethod(
                (class as *const AnyClass).cast_mut(),
                sel!(application:openURLs:),
                imp,
                types.as_ptr(),
            ) == Bool::YES
        }
    }

    /// `-[delegate application:openURLs:]`: keeps the files' paths for the window.
    #[allow(unsafe_code)]
    unsafe extern "C-unwind" fn open_urls(
        _this: *mut AnyObject,
        _cmd: Sel,
        _application: *mut AnyObject,
        urls: *mut NSArray<NSURL>,
    ) {
        // SAFETY: AppKit passes an `NSArray<NSURL>`, alive for the call (or nil).
        let Some(urls) = (unsafe { urls.as_ref() }) else {
            return;
        };
        let paths: Vec<PathBuf> = urls
            .iter()
            .filter(|url| url.isFileURL())
            .filter_map(|url| url.path())
            .map(|path| PathBuf::from(path.to_string()))
            .collect();
        if let Some(inbox) = INBOX.get() {
            inbox.push(paths);
        }
    }
}
