//! Kymo viewer: `kymo file.bdf` opens a window, `kymo` alone a file dialog.

// A GUI program on Windows, so that it opens no console window (see `cli::attach_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

use anyhow::Result;
use kymo::cli::{self, Command, USAGE};
use kymo::{app, document};

fn main() -> ExitCode {
    let console = cli::attach_console();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            cli::report_error(&document::error_text(&error), console);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    match cli::parse(std::env::args_os().skip(1))? {
        Command::Help => println!("kymo {}\n\n{USAGE}", kymo_core::version()),
        Command::Version => println!("kymo {}", kymo_core::version()),
        Command::Info(path) => {
            let document = document::open(&path)?;
            println!("{}", path.display());
            print!("{}", document.summary());
        }
        Command::View(path) => app::run(path)?,
    }
    Ok(())
}
