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

use std::io;
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
    /// Print the token stream for a Python file and exit.
    Tokenize(TokenizeArgs),
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

#[derive(Debug, Args)]
struct TokenizeArgs {
    /// The Python file to tokenize. `-` reads standard input.
    file: PathBuf,

    /// How to print the tokens.
    #[arg(long, value_enum, default_value_t = TokenFormat::Text)]
    format: TokenFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TokenFormat {
    /// One token per line, as `NAME 1,0-1,1 'x'`.
    Text,
    /// One JSON object per line.
    Json,
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
        Command::Tokenize(args) => return tokenize(args),
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

/// Print the token stream for one file.
///
/// This is a debugging command, and it is also the interface `kohebi-compat`
/// uses to diff us against CPython's `tokenize` module token for token. That
/// makes the output format a contract, described in `kohebi_parse::view`.
fn tokenize(args: &TokenizeArgs) -> ExitCode {
    let name = args.file.display().to_string();
    let source = if args.file.as_os_str() == "-" {
        io::read_to_string(io::stdin())
    } else {
        std::fs::read_to_string(&args.file)
    };
    let source = match source {
        Ok(source) => source,
        Err(error) => {
            // Not valid UTF-8 counts here. Source encoding declarations are a
            // separate job, tracked in docs/spec/03-frontend.md.
            eprintln!("kohebi: cannot read {name}: {error}");
            return ExitCode::FAILURE;
        }
    };

    match kohebi_parse::view::view(&source) {
        Ok(tokens) => {
            let out = match args.format {
                TokenFormat::Text => kohebi_parse::view::render_text(&tokens),
                TokenFormat::Json => kohebi_parse::view::render_json(&tokens),
            };
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            // The same shape CPython prints, so the text itself is comparable.
            eprint!("{}", error.report(&source, &name));
            ExitCode::FAILURE
        }
    }
}

fn init_tracing(filter: Option<&str>) {
    use tracing_subscriber::EnvFilter;
    let filter = filter.map_or_else(|| EnvFilter::new("warn"), EnvFilter::new);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
