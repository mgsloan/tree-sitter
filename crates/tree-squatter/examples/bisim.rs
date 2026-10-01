#[path = "../tests/bisim.rs"]
mod bisim;

#[cfg(not(test))]
fn main() -> std::process::ExitCode {
    bisim::run_cli()
}
