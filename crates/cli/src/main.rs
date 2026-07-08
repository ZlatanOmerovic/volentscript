//! `asr` — the AS3R compiler command-line interface.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "asr",
    version,
    about = "AS3R: ActionScript 3, revived — native AOT compiler"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compile a .as file to a native executable
    Build {
        /// Entry .as source file
        input: PathBuf,
        /// Output executable path
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Compile and immediately run a .as file
    Run {
        /// Entry .as source file
        input: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (input, output) = match cli.command {
        Command::Build { input, output } => (input, output),
        Command::Run { input } => (input, None),
    };
    match driver::build(&driver::BuildOptions { input, output }) {
        Ok(exe) => {
            println!("built {}", exe.display());
            ExitCode::SUCCESS
        }
        Err(diags) => {
            for d in &diags {
                eprintln!("{}", d.render());
            }
            ExitCode::FAILURE
        }
    }
}
