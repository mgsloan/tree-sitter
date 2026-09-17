#![allow(dead_code)]

use std::{
    ffi::OsStr,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
};
use tree_squatter_persistence::{Grammar, GrammarFingerprint, LoadedFile, Persistence};

pub fn grammar() -> Grammar {
    grammar_with_identity(42)
}

pub fn grammar_with_identity(identity: u8) -> Grammar {
    // Synthetic provider identity, paired with the packaged JSON parser.
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    Grammar::new(
        tree_sitter_squatter::Grammar::new(&language).unwrap(),
        GrammarFingerprint([identity; 32]),
    )
}

pub fn load(cache: &Persistence) -> LoadedFile {
    cache
        .load(
            Path::new("file.json"),
            &grammar(),
            &mut tree_sitter::Parser::new(),
        )
        .unwrap()
}

pub fn wait_until_killed() {
    println!("READY");
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read(&mut [0]);
}

pub struct ChildProcess(Child);
impl ChildProcess {
    pub fn start(test: &str, variable: &str, value: impl AsRef<OsStr>) -> Self {
        let mut child = Self(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--ignored", "--nocapture"])
                .env(variable, value)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut output = BufReader::new(child.0.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "child exited before readiness"
            );
            if line.trim() == "READY" {
                return child;
            }
        }
    }

    pub fn kill(mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}
impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
