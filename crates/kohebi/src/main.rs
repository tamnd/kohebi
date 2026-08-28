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

use std::fmt::Write as _;
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
    #[arg(required_unless_present = "files_from", conflicts_with = "files_from")]
    file: Option<PathBuf>,

    /// Tokenize every file named in this list, one path per line.
    ///
    /// Only `--format count` makes sense with more than one file, and reading
    /// the list from a file rather than the command line keeps a corpus of any
    /// size out of the argument limit.
    #[arg(long, value_name = "FILE")]
    files_from: Option<PathBuf>,

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
    /// One line per file, as `<tokens> <path>`. For measuring, not reading.
    Count,
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

/// Print the token stream for one file, or count the tokens in many.
///
/// This is a debugging command, and it is also the interface `kohebi-compat`
/// uses to diff us against CPython's `tokenize` module token for token. That
/// makes the output format a contract, described in `kohebi_parse::view`.
///
/// `--format count` exists for `kohebi-bench`, which times us against
/// CPython's `tokenize` module over a corpus. Timing one file per process
/// would measure process startup, so the whole corpus goes through one run.
fn tokenize(args: &TokenizeArgs) -> ExitCode {
    let files = match files(args) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("kohebi: {error}");
            return ExitCode::FAILURE;
        }
    };
    if files.len() > 1 && args.format != TokenFormat::Count {
        let name = match args.format {
            TokenFormat::Text => "text",
            TokenFormat::Json => "json",
            TokenFormat::Count => unreachable!(),
        };
        eprintln!(
            "kohebi: --format {name} describes one file, and {} were given. \
             Use --format count.",
            files.len()
        );
        return ExitCode::FAILURE;
    }

    // One buffer for the whole run. Printing per token through a locked stdout
    // makes the count format spend more time in write than in the lexer.
    let mut out = String::new();
    for file in &files {
        let name = file.display().to_string();
        let source = if file.as_os_str() == "-" {
            io::read_to_string(io::stdin())
        } else {
            std::fs::read_to_string(file)
        };
        let source = match source {
            Ok(source) => source,
            Err(error) => {
                // Not valid UTF-8 counts here. Source encoding declarations are
                // a separate job, tracked in docs/spec/03-frontend.md.
                eprintln!("kohebi: cannot read {name}: {error}");
                return ExitCode::FAILURE;
            }
        };

        match kohebi_parse::view::view(&source) {
            Ok(tokens) => match args.format {
                TokenFormat::Text => out.push_str(&kohebi_parse::view::render_text(&tokens)),
                TokenFormat::Json => out.push_str(&kohebi_parse::view::render_json(&tokens)),
                TokenFormat::Count => {
                    let _ = writeln!(out, "{} {name}", tokens.len());
                }
            },
            Err(error) => {
                print!("{out}");
                // The same shape CPython prints, so the text itself is
                // comparable. The first failure stops the run: a corpus we
                // cannot lex is a result, not something to average over.
                eprint!("{}", error.report(&source, &name));
                return ExitCode::FAILURE;
            }
        }
    }
    print!("{out}");
    ExitCode::SUCCESS
}

/// The files to tokenize, from the argument or from the list.
///
/// Blank lines in a list are skipped, because a file of paths that a shell
/// wrote almost always ends with one.
fn files(args: &TokenizeArgs) -> Result<Vec<PathBuf>, String> {
    match (&args.file, &args.files_from) {
        (Some(file), _) => Ok(vec![file.clone()]),
        (None, Some(list)) => {
            let text = std::fs::read_to_string(list)
                .map_err(|e| format!("cannot read {}: {e}", list.display()))?;
            Ok(text
                .lines()
                .map(str::trim_end)
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect())
        }
        // clap requires one of the two, so this is unreachable in practice.
        (None, None) => Err("nothing to tokenize".to_owned()),
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
