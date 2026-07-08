//! `volentscript` — the VolentScript compiler command-line interface.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "volentscript",
    version,
    about = "VolentScript: ActionScript 3, revived — native AOT compiler"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compile a .vlt file to a native executable
    Build {
        /// Entry .vlt source file
        input: PathBuf,
        /// Output executable path
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Path to libruntime.a (default: next to this executable)
        #[arg(long)]
        runtime_lib: Option<PathBuf>,
        /// Optimization level (0-3)
        #[arg(short = 'O', long = "opt", default_value = "2")]
        opt: u8,
        /// Cross-compilation target triple (e.g. x86_64-unknown-linux-gnu)
        #[arg(long)]
        target: Option<String>,
    },
    /// Compile and immediately run a .vlt file
    Run {
        /// Entry .vlt source file
        input: PathBuf,
        /// Path to libruntime.a (default: next to this executable)
        #[arg(long)]
        runtime_lib: Option<PathBuf>,
        /// Optimization level (0-3)
        #[arg(short = 'O', long = "opt", default_value = "2")]
        opt: u8,
        /// Arguments passed to the program (System.args())
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Parse a .vlt file and print its AST (compiler development aid)
    Parse {
        /// Entry .vlt source file
        input: PathBuf,
    },
    /// Type-check a .vlt file without compiling
    Check {
        /// Entry .vlt source file
        input: PathBuf,
        /// Also print the typed AST
        #[arg(long)]
        dump: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (input, output, runtime_lib, opt, target, run_args) = match cli.command {
        Command::Build {
            input,
            output,
            runtime_lib,
            opt,
            target,
        } => (input, output, runtime_lib, opt, target, None),
        Command::Run {
            input,
            runtime_lib,
            opt,
            args,
        } => (input, None, runtime_lib, opt, None, Some(args)),
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
    let opts = driver::BuildOptions {
        input,
        output,
        runtime_lib,
        target,
        opt: match opt {
            0 => driver::OptLevel::O0,
            1 => driver::OptLevel::O1,
            3 => driver::OptLevel::O3,
            _ => driver::OptLevel::O2,
        },
    };
    if let Some(run_args) = run_args {
        match driver::run(&opts, &run_args) {
            Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
            Err(errors) => report(errors),
        }
    } else {
        match driver::build(&opts) {
            Ok(exe) => {
                println!("built {}", exe.display());
                ExitCode::SUCCESS
            }
            Err(errors) => report(errors),
        }
    }
}

fn report(errors: driver::Errors) -> ExitCode {
    for rendered in &errors.rendered {
        eprintln!("{rendered}");
    }
    ExitCode::FAILURE
}
