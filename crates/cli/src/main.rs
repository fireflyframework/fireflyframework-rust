//! The `firefly` CLI binary entry point.

use clap::Parser;

use firefly_cli::cli::{run, Cli};

fn main() {
    let cli = Cli::parse();
    std::process::exit(run(cli));
}
