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
use std::path::{Path, PathBuf};
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
    /// Print the syntax tree for a Python file and exit.
    Ast(AstArgs),
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

#[derive(Debug, Args)]
struct AstArgs {
    /// The Python file to parse. `-` reads standard input.
    #[arg(required_unless_present = "files_from", conflicts_with = "files_from")]
    file: Option<PathBuf>,

    /// Parse every file named in this list, one path per line.
    #[arg(long, value_name = "FILE")]
    files_from: Option<PathBuf>,

    /// How to print the tree.
    #[arg(long, value_enum, default_value_t = AstFormat::Dump)]
    format: AstFormat,

    /// Refuse what `compile` refuses rather than what `ast.parse` accepts.
    ///
    /// The two are not the same, and the gap is wider than it sounds.
    /// `ast.parse` stops as soon as there is a tree, so `f(a=1, a=2)` and
    /// `def g(x, x): pass` both come back as ordinary trees from it and are
    /// both refused by `compile`, which runs two more passes over what the
    /// parser built. Without this flag the subcommand is `ast.parse`, which is
    /// what a tree by tree comparison wants. With it, it is `compile`, which is
    /// what asking whether CPython would run the file wants.
    #[arg(long)]
    compile: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum AstFormat {
    /// One line, exactly what `ast.dump(tree)` prints.
    Dump,
    /// The same with positions, as `ast.dump(tree, include_attributes=True)`.
    Attributes,
    /// One line per file, as `<statements> <path>`. For measuring, not reading.
    Count,
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
        Command::Ast(args) => return ast(args),
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
    let files = match files(args.file.as_ref(), args.files_from.as_ref()) {
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
        let source = match read(file, &name) {
            Ok(source) => source,
            Err(report) => {
                print!("{out}");
                eprint!("{report}");
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

/// Print the syntax tree for one file, or count the statements in many.
///
/// The other half of the comparison surface. `kohebi tokenize` is diffed
/// against CPython's `tokenize` module and this is diffed against `ast.dump`,
/// which is why the default format is what `ast.dump(tree)` prints and
/// `--format attributes` is what `ast.dump(tree, include_attributes=True)`
/// prints. A tree that agrees on shape and disagrees on positions is a tree
/// that will draw someone's error squiggle in the wrong place, so the second
/// one is the one that matters and the first is the one that is readable.
///
/// A file we cannot parse prints the traceback CPython would print and stops
/// the run, the same as `kohebi tokenize`, because a corpus we cannot parse is
/// a result rather than something to average over.
fn ast(args: &AstArgs) -> ExitCode {
    let files = match files(args.file.as_ref(), args.files_from.as_ref()) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("kohebi: {error}");
            return ExitCode::FAILURE;
        }
    };
    if files.len() > 1 && args.format != AstFormat::Count {
        let name = match args.format {
            AstFormat::Dump => "dump",
            AstFormat::Attributes => "attributes",
            AstFormat::Count => unreachable!(),
        };
        eprintln!(
            "kohebi: --format {name} describes one file, and {} were given. \
             Use --format count.",
            files.len()
        );
        return ExitCode::FAILURE;
    }

    let mut out = String::new();
    for file in &files {
        let name = file.display().to_string();
        let source = match read(file, &name) {
            Ok(source) => source,
            Err(report) => {
                print!("{out}");
                eprint!("{report}");
                return ExitCode::FAILURE;
            }
        };

        let parsed = if args.compile {
            kohebi_parse::compile_module(&source)
        } else {
            kohebi_parse::parse_module(&source)
        };
        match parsed {
            Ok(tree) => match args.format {
                AstFormat::Dump => {
                    let _ = writeln!(out, "{}", kohebi_parse::dump(&tree));
                }
                AstFormat::Attributes => {
                    let _ = writeln!(out, "{}", kohebi_parse::dump_with_attributes(&tree));
                }
                AstFormat::Count => {
                    let _ = writeln!(out, "{} {name}", statements(&tree));
                }
            },
            Err(error) => {
                print!("{out}");
                eprint!("{}", error.report(&source, &name));
                return ExitCode::FAILURE;
            }
        }
    }
    print!("{out}");
    ExitCode::SUCCESS
}

/// How many statements are in the body of a parsed module.
///
/// Only the top level, which is enough for `--format count`: the number is
/// there so that the parse has to finish and so that a run over a corpus
/// leaves a trace of having happened, not so that anyone reads it.
fn statements(tree: &kohebi_parse::ast::Mod) -> usize {
    use kohebi_parse::ast::Mod;
    match tree {
        Mod::Module { body, .. } | Mod::Interactive { body } => body.len(),
        Mod::Expression { .. } | Mod::FunctionType { .. } => 1,
    }
}

/// The files to work on, from the argument or from the list.
///
/// Blank lines in a list are skipped, because a file of paths that a shell
/// wrote almost always ends with one.
fn files(file: Option<&PathBuf>, list: Option<&PathBuf>) -> Result<Vec<PathBuf>, String> {
    match (file, list) {
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
        (None, None) => Err("nothing to read".to_owned()),
    }
}

/// The text of one file, in whatever encoding the file says it is in.
///
/// `-` is standard input. What encoding the bytes are in is the file's own
/// business to declare, which is PEP 263 and lives in `kohebi_parse::source`,
/// so a failure here is a `SyntaxError` with a position in it like any other
/// and gets printed the same way.
fn read(file: &Path, name: &str) -> Result<String, String> {
    let bytes = if file.as_os_str() == "-" {
        let mut buffer = Vec::new();
        io::Read::read_to_end(&mut io::stdin(), &mut buffer).map(|_| buffer)
    } else {
        std::fs::read(file)
    };
    let bytes = bytes.map_err(|error| format!("kohebi: cannot read {name}: {error}\n"))?;
    kohebi_parse::decode(&bytes)
        .map(|source| source.text)
        .map_err(|error| error.error.report(&error.text, name))
}

fn init_tracing(filter: Option<&str>) {
    use tracing_subscriber::EnvFilter;
    let filter = filter.map_or_else(|| EnvFilter::new("warn"), EnvFilter::new);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
