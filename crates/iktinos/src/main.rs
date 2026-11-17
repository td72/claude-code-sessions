//! `iktinos`: the command.
//!
//! This crate is the way in and nothing else. What the command does lives in
//! the crates it starts: without a subcommand, the terminal viewer
//! (`iktinos-tui`), over the model of the sessions (`iktinos-core`) and, where
//! the bridge mod is installed, its live events (`iktinos-live`).
//!
//! Up to 0.2 the crate of this name was the library that is now
//! `iktinos-core`.

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    // A subcommand would be told from the first argument, here. There is
    // none yet: everything is the viewer's.
    iktinos_tui::run(env!("CARGO_PKG_VERSION"), env::args().skip(1))
}
