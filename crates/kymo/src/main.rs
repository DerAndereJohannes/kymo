//! Kymo viewer: `kymo file.bdf` opens a window, `kymo` alone a file dialog.

mod app;
mod cli;
mod document;

use std::process::ExitCode;

use anyhow::Result;

use crate::cli::{Command, USAGE};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kymo: {}", document::error_text(&error));
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
