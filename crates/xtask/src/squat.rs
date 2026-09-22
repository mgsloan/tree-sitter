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
            let status = Process::new("cargo")
                .args([
                    "test",
                    "-p",
                    "tree-squatter",
                    "-p",
                    "tree-squatter-persistence",
                    "-p",
                    "corpus-analysis",
                    "-p",
                    "squatter-bench",
                    "-p",
                    "xtask",
                ])
                .current_dir(crate::root_dir())
                .status()
                .context("run cargo")?;
            ensure!(status.success(), "cargo failed: {status}");
            Ok(())
        }
        Command::Test(test) => run::run(test.options, Some(test.level)),
        Command::Bench(options) => run::run(options, None),
    }
}
