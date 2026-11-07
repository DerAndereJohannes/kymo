//! Kymo viewer binary. The GUI arrives in phase 2; for now this only checks its arguments.

use anyhow::{Result, bail};

const USAGE: &str = "Usage: kymo <file.edf|file.bdf>";

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next()) {
        (Some(arg), None) if arg == "-h" || arg == "--help" => {
            println!("kymo {}\n{USAGE}", kymo_core::version());
            Ok(())
        }
        (Some(path), None) => bail!(
            "cannot open {}: the viewer is not implemented yet",
            std::path::Path::new(&path).display()
        ),
        _ => bail!("{USAGE}"),
    }
}
