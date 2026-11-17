//! PyxeeTB. Phase 0 ships one working command, `probe`: it measures the machine it runs on and its path to
//! Twitch, so the design decisions that depend on the server (region, CPU throttling) are made from numbers.

mod cli;
mod probe;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args) {
        Ok(cli::Command::Help) => {
            print!("{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        Ok(cli::Command::Version) => {
            println!("pyxeetb {}", pyxeetb_core::VERSION);
            ExitCode::SUCCESS
        }
        Ok(cli::Command::Probe(options)) => {
            let text = probe::run(&options);
            print!("{text}");
            match std::fs::write(&options.out, &text) {
                Ok(()) => {
                    println!("\nReport written to {}", options.out);
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("\nCould not write {}: {err}", options.out);
                    ExitCode::FAILURE
                }
            }
        }
        Err(message) => {
            eprintln!("error: {message}\n\n{}", cli::USAGE);
            ExitCode::from(2)
        }
    }
}
