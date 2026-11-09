//! Command-line arguments.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

pub const USAGE: &str = "\
Usage: kymo [FILE]          open FILE in a window (a file dialog without FILE)
       kymo --info FILE     print a summary of FILE and exit
       kymo -h | --help     show this help
       kymo -V | --version  show the version";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    /// Open a window, with a file or with a file dialog.
    View(Option<PathBuf>),
    /// Print a summary of a file without opening a window.
    Info(PathBuf),
}

/// Arguments that don't form a valid command.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n\n{USAGE}", self.0)
    }
}

impl std::error::Error for UsageError {}

/// Parses the arguments after the program name. `--` ends the options, so a file name may
/// start with `-`.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, UsageError> {
    let mut info = false;
    let mut paths = Vec::new();
    let mut options_done = false;
    for arg in args {
        if options_done || !arg.to_string_lossy().starts_with('-') || arg == "-" {
            paths.push(PathBuf::from(arg));
            continue;
        }
        match arg.to_string_lossy().as_ref() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--info" => info = true,
            "--" => options_done = true,
            other => return Err(UsageError(format!("unknown option {other}"))),
        }
    }
    if paths.len() > 1 {
        return Err(UsageError("expected at most one file".into()));
    }
    match (info, paths.pop()) {
        (true, Some(path)) => Ok(Command::Info(path)),
        (true, None) => Err(UsageError("--info needs a file".into())),
        (false, path) => Ok(Command::View(path)),
    }
}

/// Lets text printed by the command line reach a terminal. On Windows, Kymo is a GUI
/// program, so that starting it from Explorer opens no console window; started from a
/// terminal, it attaches to the terminal's console, unless its output is redirected (to a
/// pipe or file, which works as is). The terminal doesn't wait for a GUI program, so what
/// it prints may come after the next prompt. Returns whether printed text reaches anyone;
/// elsewhere it always does.
pub fn attach_console() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType,
        };
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        // SAFETY: `GetFileType` only reads the handle, which std got from the process's
        // standard handles (null if there is none, which it reports as unknown);
        // `AttachConsole` takes no pointers and, without a parent console, fails and
        // changes nothing.
        #[allow(unsafe_code)]
        unsafe {
            let redirected = |handle: std::os::windows::io::RawHandle| {
                matches!(GetFileType(handle), FILE_TYPE_PIPE | FILE_TYPE_DISK)
            };
            if redirected(std::io::stdout().as_raw_handle())
                || redirected(std::io::stderr().as_raw_handle())
            {
                return true;
            }
            AttachConsole(ATTACH_PARENT_PROCESS) != 0
        }
    }
    #[cfg(not(windows))]
    true
}

/// Reports an error that ends the program: on standard error, and in a message box if
/// nobody would see that (Windows, started from Explorer; see [`attach_console`]).
pub fn report_error(text: &str, console: bool) {
    eprintln!("kymo: {text}");
    if !console {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Kymo")
            .set_description(text)
            .show();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Command, UsageError> {
        parse(args.iter().map(OsString::from))
    }

    fn view(path: &str) -> Result<Command, UsageError> {
        Ok(Command::View(Some(path.into())))
    }

    #[test]
    fn no_arguments_open_a_file_dialog() {
        assert_eq!(run(&[]), Ok(Command::View(None)));
    }

    #[test]
    fn a_file_opens_in_a_window() {
        assert_eq!(run(&["rec.bdf"]), view("rec.bdf"));
        assert_eq!(
            run(&["dir/with space/Rec.EDF"]),
            view("dir/with space/Rec.EDF")
        );
    }

    #[test]
    fn info_prints_a_file() {
        assert_eq!(run(&["--info", "a.edf"]), Ok(Command::Info("a.edf".into())));
        assert_eq!(run(&["a.edf", "--info"]), Ok(Command::Info("a.edf".into())));
        assert!(run(&["--info"]).is_err());
    }

    #[test]
    fn help_and_version_win_over_everything_else() {
        assert_eq!(run(&["-h"]), Ok(Command::Help));
        assert_eq!(run(&["a.edf", "--help"]), Ok(Command::Help));
        assert_eq!(run(&["--info", "-h"]), Ok(Command::Help));
        assert_eq!(run(&["-V"]), Ok(Command::Version));
        assert_eq!(run(&["--version", "a.edf", "b.edf"]), Ok(Command::Version));
    }

    #[test]
    fn rejects_unknown_options_and_extra_files() {
        let err = run(&["--frobnicate", "a.edf"]).unwrap_err();
        assert!(err.0.contains("--frobnicate"), "{err}");
        assert!(err.to_string().contains("Usage:"), "{err}");
        assert!(run(&["a.edf", "b.edf"]).is_err());
        assert!(run(&["--info", "a.edf", "b.edf"]).is_err());
    }

    #[test]
    fn double_dash_ends_options() {
        assert_eq!(run(&["--", "-odd.edf"]), view("-odd.edf"));
        assert_eq!(run(&["--", "--help"]), view("--help"));
        assert_eq!(run(&["-"]), view("-"));
    }

    #[cfg(unix)]
    #[test]
    fn keeps_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt;

        let raw = OsString::from_vec(b"caf\xe9.edf".to_vec());
        assert_eq!(
            parse([raw.clone()]),
            Ok(Command::View(Some(PathBuf::from(raw))))
        );
    }
}
