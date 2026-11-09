//! The system's app mode System follows: the desktop portal's color scheme on Linux, the
//! registry on Windows.

use kymo::app_mode::color_scheme_dark;

#[test]
fn the_portals_color_scheme_is_dark_only_when_it_says_so() {
    // 0 no preference, 1 prefer dark, 2 prefer light; GNOME's default is 0, and light.
    assert_eq!(color_scheme_dark(0), Some(false));
    assert_eq!(color_scheme_dark(1), Some(true));
    assert_eq!(color_scheme_dark(2), Some(false));
    assert_eq!(color_scheme_dark(3), None);
    assert_eq!(color_scheme_dark(u32::MAX), None);
}

/// A fake desktop portal on one end of a socket pair, Kymo's watcher on the other: no bus
/// or desktop needed.
#[cfg(all(unix, not(target_os = "macos")))]
mod portal {
    use kymo::app_mode::AppMode;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};
    use zbus::blocking::{Connection, connection::Builder};
    use zbus::zvariant::Value;

    const PATH: &str = "/org/freedesktop/portal/desktop";
    const INTERFACE: &str = "org.freedesktop.portal.Settings";

    /// The scheme of a portal that doesn't have the setting.
    const NOT_SUPPORTED: u32 = u32::MAX;

    /// What the portal's `Settings` interface answers for `Read`.
    struct Portal(Arc<AtomicU32>);

    #[zbus::interface(name = "org.freedesktop.portal.Settings")]
    impl Portal {
        /// The value in a second variant, as portals send it.
        fn read(&self, namespace: &str, key: &str) -> zbus::fdo::Result<Value<'static>> {
            let scheme = self.0.load(Ordering::SeqCst);
            if (namespace, key) == ("org.freedesktop.appearance", "color-scheme")
                && scheme != NOT_SUPPORTED
            {
                Ok(Value::Value(Box::new(Value::U32(scheme))))
            } else {
                Err(zbus::fdo::Error::Failed("no such setting".into()))
            }
        }
    }

    /// The watcher's end and the portal's, which emits the signals; `portal` is whether the
    /// portal's end has the `Settings` object at all.
    fn connect(scheme: &Arc<AtomicU32>, portal: bool) -> (Connection, Connection) {
        let (client, server) = UnixStream::pair().unwrap();
        let scheme = Arc::clone(scheme);
        // The handshakes wait for each other.
        let server = std::thread::spawn(move || {
            let mut builder = Builder::async_io_unix_stream(server)
                .p2p()
                .server(zbus::Guid::generate())
                .unwrap();
            if portal {
                builder = builder.serve_at(PATH, Portal(scheme)).unwrap();
            }
            builder.build().unwrap()
        });
        let client = Builder::async_io_unix_stream(client).p2p().build().unwrap();
        (client, server.join().unwrap())
    }

    fn changed(server: &Connection, namespace: &str, key: &str, value: u32) {
        server
            .emit_signal(
                None::<&str>,
                PATH,
                INTERFACE,
                "SettingChanged",
                &(namespace, key, Value::U32(value)),
            )
            .unwrap();
    }

    fn wait_for(mode: &AppMode, dark: Option<bool>) {
        let until = Instant::now() + Duration::from_secs(10);
        while mode.dark() != dark {
            assert!(
                Instant::now() < until,
                "still {:?}, not {dark:?}",
                mode.dark()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_mode_is_read_at_once_and_follows_the_portal() {
        let scheme = Arc::new(AtomicU32::new(1));
        let (client, server) = connect(&scheme, true);
        let mode = AppMode::watch_connection(client, &eframe::egui::Context::default());
        // Known when the first frame comes, not some time after.
        assert_eq!(mode.dark(), Some(true));

        changed(&server, "org.freedesktop.appearance", "color-scheme", 2);
        wait_for(&mode, Some(false));
        changed(&server, "org.freedesktop.appearance", "color-scheme", 1);
        wait_for(&mode, Some(true));
        // No preference is light; a value that isn't known is unknown.
        changed(&server, "org.freedesktop.appearance", "color-scheme", 0);
        wait_for(&mode, Some(false));
        changed(&server, "org.freedesktop.appearance", "color-scheme", 7);
        wait_for(&mode, None);
    }

    #[test]
    fn other_settings_are_ignored() {
        let scheme = Arc::new(AtomicU32::new(2));
        let (client, server) = connect(&scheme, true);
        let mode = AppMode::watch_connection(client, &eframe::egui::Context::default());
        assert_eq!(mode.dark(), Some(false));

        // Each would make it dark, were it taken for the color scheme.
        changed(&server, "org.freedesktop.appearance", "accent-color", 1);
        changed(&server, "org.freedesktop.appearance", "contrast", 1);
        changed(&server, "org.gnome.desktop.interface", "color-scheme", 1);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(mode.dark(), Some(false));
        // The signals had arrived; the one that counts does follow.
        changed(&server, "org.freedesktop.appearance", "color-scheme", 1);
        wait_for(&mode, Some(true));
    }

    #[test]
    fn a_portal_without_the_setting_leaves_the_mode_unknown() {
        let scheme = Arc::new(AtomicU32::new(NOT_SUPPORTED));
        let (client, _server) = connect(&scheme, true);
        let started = Instant::now();
        let mode = AppMode::watch_connection(client, &eframe::egui::Context::default());
        assert_eq!(mode.dark(), None);
        // The error ends the wait, which is otherwise half a second at most.
        assert!(started.elapsed() < Duration::from_millis(400));
    }

    #[test]
    fn a_portal_that_never_answers_delays_the_start_by_half_a_second_at_most() {
        let scheme = Arc::new(AtomicU32::new(1));
        let (client, _server) = connect(&scheme, false);
        let started = Instant::now();
        let mode = AppMode::watch_connection(client, &eframe::egui::Context::default());
        assert_eq!(mode.dark(), None);
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}

/// Windows: the app mode is read from the registry and followed as it changes there. Only
/// on CI, as it switches every app's mode for a moment.
#[cfg(windows)]
#[test]
fn the_app_mode_follows_the_registry() {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use eframe::egui;
    use kymo::app_mode::AppMode;
    if std::env::var_os("CI").is_none() {
        eprintln!("skipped: changes the app mode, so only on CI");
        return;
    }
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
    let reg = |args: &[&str]| {
        Command::new("reg")
            .args(args)
            .output()
            .expect("reg runs")
            .status
            .success()
    };
    let write = |light: u32| {
        let data = light.to_string();
        let args = [
            "add",
            KEY,
            "/v",
            "AppsUseLightTheme",
            "/t",
            "REG_DWORD",
            "/d",
            &data,
            "/f",
        ];
        assert!(reg(&args), "reg add {light}");
    };
    let until = |mode: &AppMode, dark: bool| {
        let start = Instant::now();
        while mode.dark() != Some(dark) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "still {:?}",
                mode.dark()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    write(0);
    let mode = AppMode::watch(&egui::Context::default());
    assert_eq!(mode.dark(), Some(true));
    write(1);
    until(&mode, false);
    write(0);
    until(&mode, true);
    write(1);
    until(&mode, false);
}
