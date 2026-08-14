use clap::Parser;

use hd_movies::app;
use hd_movies::cli::Cli;

fn main() {
    if let Err(error) = app::run(Cli::parse()) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
