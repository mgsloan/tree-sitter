mod run;

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand, ValueEnum};
use std::process::Command as Process;

#[derive(Subcommand)]
pub enum Command {
    /// Run hermetic checks or shared corpus checks.
    Test(Test),
    /// Validate and measure the selected workloads.
    Bench(run::Options),
}

#[derive(Clone, Copy, ValueEnum, PartialEq)]
pub enum TestLevel {
    Quick,
    Corpus,
    Sanitize,
}

#[derive(Args)]
pub struct Test {
    #[arg(value_enum, default_value = "quick")]
    level: TestLevel,
    #[command(flatten)]
    options: run::Options,
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Test(Test {
            level: TestLevel::Quick,
            ..
        }) => {
            for (program, arguments) in [
                (
                    "cargo",
                    vec![
                        "test",
                        "-p",
                        "tree-sitter-squatter",
                        "-p",
                        "tree-squatter-persistence",
                        "-p",
                        "corpus-analysis",
                        "-p",
                        "squatter-bench",
                        "-p",
                        "xtask",
                    ],
                ),
                ("make", vec!["-C", "lib/squat", "check"]),
            ] {
                let status = Process::new(program)
                    .args(arguments)
                    .current_dir(crate::root_dir())
                    .status()
                    .with_context(|| format!("run {program}"))?;
                ensure!(status.success(), "{program} failed: {status}");
            }
            Ok(())
        }
        Command::Test(test) => run::run(test.options, Some(test.level)),
        Command::Bench(options) => run::run(options, None),
    }
}
