//! Command-line front end of the synthetic EDF/BDF generator.

use anyhow::{Result, bail};

const HELP: &str = "\
kymo-testgen: generate synthetic EDF/BDF files for tests and benchmarks

Usage: kymo-testgen [OPTIONS]

Options:
  -h, --help  Print this help

No generators are implemented yet.";

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return Ok(());
    }
    bail!("no generators implemented yet; see --help")
}
