//! The kohebi command line driver.
//!
//! Two modes, per `docs/spec/00-README.md`:
//!
//! - `kohebi run app.py` executes under the tiered JIT.
//! - `kohebi build app.py` emits a Rust crate, hands it to `rustc`, and
//!   produces a native binary.
//!
//! Nothing is implemented yet. The command surface is here first so that the
//! shape of the tool is reviewable before any of it works, and so the flags
//! named throughout the spec have exactly one definition.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "kohebi",
    version,
    about = "A Python runtime written in Rust",
    long_about = None,
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Log filter, e.g. `kohebi_jit=debug`.
    #[arg(long, global = true, env = "KOHEBI_LOG")]
    log: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a Python program under the tiered JIT.
    Run(RunArgs),
    /// Compile a Python program to a native binary by way of Rust.
    Build(BuildArgs),
    /// Print the resolved configuration and exit.
    Config,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// The Python file to execute.
    script: PathBuf,

    /// Arguments passed through to the program as `sys.argv[1:]`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    argv: Vec<String>,

    /// Highest execution tier to use. Lower tiers are for debugging.
    #[arg(long, value_enum, default_value_t = Tier::T2)]
    max_tier: Tier,

    /// Record a profile for `kohebi build --profile` to consume.
    #[arg(long, value_name = "FILE")]
    profile_out: Option<PathBuf>,

    /// Collect at every safepoint. Very slow. See docs/spec/12-testing.md.
    #[arg(long)]
    gc_stress: bool,

    /// Fail every guard on first execution. Very slow.
    #[arg(long)]
    deopt_stress: bool,

    /// Report why and where deoptimization happened.
    #[arg(long)]
    deopt_stats: bool,
}

#[derive(Debug, Args)]
struct BuildArgs {
    /// The Python entry point to compile.
    script: PathBuf,

    /// Output binary path.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// How much of the program the compiler may assume will not change.
    #[arg(long, value_enum, default_value_t = Sealing::Sealed)]
    sealing: Sealing,

    /// Consume a profile recorded by `kohebi run --profile-out`.
    #[arg(long, value_name = "FILE")]
    profile: Option<PathBuf>,

    /// Write the generated Rust crate here and stop before invoking rustc.
    #[arg(long, value_name = "DIR")]
    emit_rust: Option<PathBuf>,

    /// Skip rustc entirely and emit machine code through the tier 2 backend.
    /// Much faster to build, somewhat slower to run, no toolchain required.
    #[arg(long)]
    fast: bool,

    /// Target triple. Defaults to the host.
    #[arg(long, value_name = "TRIPLE")]
    target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Tier {
    /// Interpreter only.
    T0,
    /// Interpreter and baseline JIT.
    T1,
    /// All tiers.
    T2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Sealing {
    /// Everything stays patchable. Slowest, and fully dynamic.
    Open,
    /// Assume the program does not monkeypatch itself, and deoptimize if it does.
    Sealed,
    /// Assume it and do not check. Fastest, and it can diverge from `CPython`.
    Frozen,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.log.as_deref());

    let unimplemented = match &cli.command {
        Command::Run(_) => "kohebi run",
        Command::Build(_) => "kohebi build",
        Command::Config => {
            println!("kohebi {}", env!("CARGO_PKG_VERSION"));
            println!("rustc target: {}", std::env::consts::ARCH);
            println!("status:       scaffolding, nothing is implemented");
            println!("design:       docs/spec/00-README.md");
            return ExitCode::SUCCESS;
        }
    };

    eprintln!(
        "kohebi: {unimplemented} is not implemented yet.\n\
         \n\
         This repository currently holds the design and the crate structure it\n\
         implies. Start at docs/spec/00-README.md, and docs/spec/10-milestones.md\n\
         for what has to happen before this command does anything."
    );
    ExitCode::FAILURE
}

fn init_tracing(filter: Option<&str>) {
    use tracing_subscriber::EnvFilter;
    let filter = filter.map_or_else(|| EnvFilter::new("warn"), EnvFilter::new);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
