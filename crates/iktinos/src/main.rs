//! `iktinos`: the command.
//!
//! This crate is the way in and nothing else. What the command does lives in
//! the crates it starts, over the model of the sessions (`iktinos-core`) and,
//! where the bridge mod is installed, its live events (`iktinos-live`):
//!
//! - `iktinos`: the terminal viewer (`iktinos-tui`)
//! - `iktinos serve`: a local server for a browser (`iktinos-serve`)
//!
//! Up to 0.2 the crate of this name was the library that is now
//! `iktinos-core`.

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args().skip(1).peekable();
    // A subcommand is told from the first argument. Without one,
    // everything is the viewer's.
    if args.peek().is_some_and(|first| first == "serve") {
        return iktinos_serve::run(args.skip(1));
    }
    iktinos_tui::run(env!("CARGO_PKG_VERSION"), args)
}
