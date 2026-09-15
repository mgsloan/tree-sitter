use std::{path::PathBuf, process::Command as Process};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};

use crate::root_dir;

#[derive(Subcommand)]
pub enum Command {
    /// Run Squatter correctness checks.
    Test(Test),
    /// Stage a corpus and run the supported benchmark matrix.
    Bench(Bench),
}

#[derive(Clone, Copy, ValueEnum)]
enum TestLevel {
    /// Hermetic Rust, native C, and Python unit tests.
    Quick,
    /// Cross-grammar corpus comparisons in the pinned container.
    Corpus,
    /// The corpus comparisons under `ASan` and `UBSan`.
    Sanitize,
}

#[derive(Args)]
pub struct Test {
    #[arg(value_enum, default_value = "quick")]
    level: TestLevel,
    /// Fresh output directory for corpus and sanitizer checks.
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long, default_value = "../../code-corpora")]
    code_corpora: PathBuf,
    /// Cached build image ID to use instead of the code-corpora lock.
    #[arg(long)]
    image: Option<String>,
    /// Limit corpus checks to these grammars.
    #[arg(long)]
    grammar: Vec<String>,
}

#[derive(Args)]
pub struct Bench {
    /// Arguments forwarded to tools/squatter/run.py.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    arguments: Vec<String>,
}

fn execute(program: &str, arguments: &[&str]) -> Result<()> {
    let status = Process::new(program)
        .args(arguments)
        .current_dir(root_dir())
        .status()
        .with_context(|| format!("run {program}"))?;
    if !status.success() {
        bail!("{program} {} failed with {status}", arguments.join(" "));
    }
    Ok(())
}

fn quick() -> Result<()> {
    execute(
        "cargo",
        &[
            "test",
            "-p",
            "tree-sitter-squatter",
            "-p",
            "tree-squatter-persistence",
            "-p",
            "corpus-analysis",
            "-p",
            "squatter-bench",
        ],
    )?;
    execute("make", &["-C", "lib/squat", "check"])?;
    execute(
        "python3",
        &[
            "-m",
            "unittest",
            "discover",
            "-s",
            "tools/memory-pareto",
            "-p",
            "test_*.py",
        ],
    )
}

fn corpus(arguments: &Test, sanitize: bool) -> Result<()> {
    let output = arguments
        .output
        .as_ref()
        .context("--output is required for corpus and sanitize checks")?;
    let native_output = output.join("native");
    let mut native = Process::new("python3");
    native
        .current_dir(root_dir())
        .arg("lib/squat/tests/container.py")
        .arg("--code-corpora")
        .arg(&arguments.code_corpora)
        .arg("--output")
        .arg(&native_output)
        .arg("--queries");
    if sanitize {
        native.arg("--sanitize");
    }
    if let Some(image) = &arguments.image {
        native.arg("--image").arg(image);
    }
    for grammar in &arguments.grammar {
        native.arg("--grammar").arg(grammar);
    }
    let status = native
        .status()
        .context("run native Squatter corpus checks")?;
    if !status.success() {
        bail!("native Squatter corpus checks failed with {status}");
    }
    if sanitize {
        return Ok(());
    }
    let rust_output = output.join("rust");
    let mut rust = Process::new("python3");
    rust.current_dir(root_dir())
        .arg("tools/squatter/run.py")
        .arg("--code-corpora")
        .arg(&arguments.code_corpora)
        .arg("--output")
        .arg(&rust_output)
        .args(["--checks-only", "--skip-layouts", "--per-bucket", "1"]);
    if let Some(image) = &arguments.image {
        rust.arg("--image").arg(image);
    }
    for grammar in &arguments.grammar {
        rust.arg("--grammar").arg(grammar);
    }
    let status = rust.status().context("run Rust Squatter corpus checks")?;
    if !status.success() {
        bail!("Rust Squatter corpus checks failed with {status}");
    }
    Ok(())
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Test(arguments) => match arguments.level {
            TestLevel::Quick => quick(),
            TestLevel::Corpus => corpus(&arguments, false),
            TestLevel::Sanitize => corpus(&arguments, true),
        },
        Command::Bench(arguments) => {
            let status = Process::new("python3")
                .arg("tools/squatter/run.py")
                .args(arguments.arguments)
                .current_dir(root_dir())
                .status()
                .context("run Squatter benchmark driver")?;
            if !status.success() {
                bail!("Squatter benchmark driver failed with {status}");
            }
            Ok(())
        }
    }
}
