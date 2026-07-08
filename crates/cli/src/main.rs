//! `vigorscript` — the VigorScript compiler command-line interface.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "vigorscript",
    version,
    about = "VigorScript: ActionScript 3, revived — native AOT compiler"
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
    /// Parse a .as file and print its AST (compiler development aid)
    Parse {
        /// Entry .as source file
        input: PathBuf,
    },
    /// Type-check a .as file without compiling
    Check {
        /// Entry .as source file
        input: PathBuf,
        /// Also print the typed AST
        #[arg(long)]
        dump: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (input, output) = match cli.command {
        Command::Build { input, output } => (input, output),
        Command::Run { input } => (input, None),
        Command::Parse { input } => {
            return match driver::parse_dump(&input) {
                Ok(dump) => {
                    print!("{dump}");
                    ExitCode::SUCCESS
                }
                Err(errors) => report(errors),
            };
        }
        Command::Check { input, dump } => {
            return match driver::check(&input) {
                Ok(result) => {
                    for w in &result.warnings {
                        eprintln!("{w}");
                    }
                    if dump {
                        print!("{}", result.dump);
                    } else {
                        println!("ok");
                    }
                    ExitCode::SUCCESS
                }
                Err(errors) => report(errors),
            };
        }
    };
    match driver::build(&driver::BuildOptions { input, output }) {
        Ok(exe) => {
            println!("built {}", exe.display());
            ExitCode::SUCCESS
        }
        Err(errors) => report(errors),
    }
}

fn report(errors: driver::Errors) -> ExitCode {
    for rendered in &errors.rendered {
        eprintln!("{rendered}");
    }
    ExitCode::FAILURE
}
