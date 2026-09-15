use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use corpus_analysis::{Input, LoadedGrammar, Registry, digest, inventory, parse, seed_for, select};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    time::Duration,
};

#[derive(Parser)]
#[command(about = "Deterministic Tree-sitter corpus sampling and storage experiments")]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Sample(Sample),
    #[command(trailing_var_arg = true)]
    MemoryPareto {
        #[arg(allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
}
#[derive(clap::Args)]
struct Sample {
    #[arg(long, default_value = "../../code-corpora")]
    code_corpora: PathBuf,
    #[arg(long)]
    registry: Option<PathBuf>,
    #[arg(long, default_value = "/opt/corpus/grammars")]
    grammar_root: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 42)]
    seed: u64,
    #[arg(long, default_value_t = 100)]
    per_bucket: usize,
    #[arg(long)]
    repo: Vec<String>,
    #[arg(long)]
    count: Option<usize>,
    #[arg(long, default_value_t = 16 * 1024 * 1024)]
    max_file_bytes: u64,
}

#[derive(Clone, Default)]
struct Features {
    edges: BTreeMap<(u16, u16, u16), u32>,
    missing: BTreeSet<u16>,
}
impl Features {
    fn extract(tree: &tree_sitter::Tree) -> Self {
        let mut features = Self::default();
        let mut cursor = tree.walk();
        let mut parents = Vec::new();
        loop {
            let node = cursor.node();
            if node.is_missing() {
                features.missing.insert(node.kind_id());
            }
            if let Some(parent) = parents.last() {
                let edge = (
                    *parent,
                    cursor.field_id().map_or(0, Into::into),
                    node.kind_id(),
                );
                features
                    .edges
                    .entry(edge)
                    .and_modify(|arity| *arity = (*arity).max(node.child_count()))
                    .or_insert(node.child_count());
            }
            if cursor.goto_first_child() {
                parents.push(node.kind_id());
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return features;
                }
                parents.pop();
            }
        }
    }
    fn adds_to(&self, previous: &Self) -> bool {
        self.missing
            .iter()
            .any(|symbol| !previous.missing.contains(symbol))
            || self.edges.iter().any(|(edge, arity)| {
                previous.edges.get(edge).is_none_or(|old| {
                    [10, 50, 100]
                        .iter()
                        .any(|threshold| old <= threshold && arity > threshold)
                })
            })
    }
    fn merge(&mut self, other: &Self) {
        self.missing.extend(&other.missing);
        for (&edge, &arity) in &other.edges {
            self.edges
                .entry(edge)
                .and_modify(|old| *old = (*old).max(arity))
                .or_insert(arity);
        }
    }
}

fn bucket(bytes: u64, nodes: usize) -> Option<&'static str> {
    if nodes < 10 {
        Some("tiny")
    } else if bytes < 4096 {
        Some("small")
    } else if bytes <= 100 * 1024 {
        Some("normal")
    } else if bytes > 1024 * 1024 {
        Some("large")
    } else {
        None
    }
}
fn split(input: &Input) -> &'static str {
    if input.path.starts_with("test/") {
        "test"
    } else {
        "train"
    }
}

fn sample(arguments: Sample) -> Result<()> {
    ensure!(arguments.per_bucket > 0, "--per-bucket must be positive");
    ensure!(!arguments.output.exists(), "output already exists");
    let registry = if let Some(path) = &arguments.registry {
        Registry::read(path)?
    } else {
        Registry::from_artifacts(&arguments.grammar_root)?
    };
    let inventory = inventory(
        &arguments.code_corpora,
        &registry,
        &arguments.repo,
        arguments.max_file_bytes,
    );
    fs::create_dir_all(&arguments.output)?;
    fs::write(
        arguments.output.join("inventory.json"),
        serde_json::to_vec_pretty(&inventory)?,
    )?;
    let inputs = select(
        inventory.inputs,
        arguments.count,
        arguments.seed,
        "sample-inventory",
    );
    let mut loaded = BTreeMap::new();
    let mut buckets: BTreeMap<String, Vec<(Input, Features)>> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut hashes = BTreeMap::new();
    for input in &inputs {
        let result = (|| -> Result<_> {
            if !loaded.contains_key(&input.grammar) {
                loaded.insert(
                    input.grammar.clone(),
                    // The registry identifies trusted grammar exports. This map
                    // outlives every parser/tree/query created below.
                    unsafe { LoadedGrammar::open(&registry.grammars[&input.grammar])? },
                );
            }
            let bytes = fs::read(arguments.code_corpora.join(&input.path))?;
            hashes.insert(input.path.clone(), digest(&bytes));
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&loaded[&input.grammar].language)?;
            let tree = parse(&mut parser, &bytes, Duration::from_secs(30))?;
            Ok((
                tree.root_node().descendant_count(),
                Features::extract(&tree),
            ))
        })();
        match result {
            Ok((nodes, features)) => {
                if let Some(bucket) = bucket(input.bytes, nodes) {
                    let name = format!("{}-{bucket}", split(input));
                    let entries = buckets.entry(name.clone()).or_default();
                    entries.push((input.clone(), features));
                    entries.sort_by_key(|(entry, _)| seed_for(arguments.seed, &name, &entry.path));
                    entries.truncate(arguments.per_bucket);
                }
            }
            Err(error) => failures.push(format!("{}: {error:#}", input.path)),
        }
    }
    let mut seen: BTreeMap<(String, String), Features> = BTreeMap::new();
    let mut selected = BTreeSet::new();
    for entries in buckets.values() {
        for (input, features) in entries {
            // Large files do not establish the baseline for unusual sampling.
            if input.bytes <= 100 * 1024 {
                seen.entry((split(input).into(), input.grammar.clone()))
                    .or_default()
                    .merge(features);
            }
            selected.insert(input.path.clone());
        }
    }
    // Reparse only unselected files. Keeping every file's edge map would make
    // corpus-size memory use dwarf the trees being analyzed.
    for input in &inputs {
        if selected.contains(&input.path) || !loaded.contains_key(&input.grammar) {
            continue;
        }
        let bytes = match fs::read(arguments.code_corpora.join(&input.path)) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        if hashes.get(&input.path) != Some(&digest(&bytes)) {
            failures.push(format!("{} changed during sampling", input.path));
            continue;
        }
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&loaded[&input.grammar].language)?;
        let tree = match parse(&mut parser, &bytes, Duration::from_secs(30)) {
            Ok(tree) => tree,
            Err(error) => {
                failures.push(format!("{}: {error:#}", input.path));
                continue;
            }
        };
        let features = Features::extract(&tree);
        let baseline = seen
            .entry((split(input).into(), input.grammar.clone()))
            .or_default();
        if features.adds_to(baseline) {
            baseline.merge(&features);
            buckets
                .entry(format!("{}-unusual", split(input)))
                .or_default()
                .push((input.clone(), features));
        }
    }
    let mut counts = BTreeMap::new();
    for split in ["train", "test"] {
        for bucket in ["tiny", "small", "normal", "large", "unusual"] {
            let name = format!("{split}-{bucket}");
            let entries = buckets.entry(name.clone()).or_default();
            entries.sort_by(|a, b| a.0.path.cmp(&b.0.path));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(arguments.output.join(&name))?;
            for (input, _) in entries.iter() {
                writeln!(file, "{}", input.path)?;
            }
            counts.insert(name, entries.len());
        }
    }
    let report = serde_json::json!({
        "seed": arguments.seed, "planned": inputs.len(), "failures": failures,
        "counts": counts, "registry": registry, "input_sha256": hashes,
        "complete": failures.is_empty(), "repository_filter": arguments.repo,
        "count_limit": arguments.count, "per_bucket": arguments.per_bucket,
        "unusual_rule": "new parent/field/child edge, child arity crossing >10/>50/>100, or missing symbol",
    });
    fs::write(
        arguments.output.join("run.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&counts)?);
    ensure!(
        failures.is_empty(),
        "{} sampling failures; see run.json",
        failures.len()
    );
    Ok(())
}

fn main() -> Result<()> {
    match Arguments::parse().command {
        Command::Sample(arguments) => sample(arguments),
        Command::MemoryPareto { arguments } => corpus_analysis::pareto::run(&arguments),
    }
}

#[cfg(test)]
mod sampling_tests {
    use super::*;
    #[test]
    fn exact_buckets_and_intentional_gap() {
        assert_eq!(bucket(4095, 10), Some("small"));
        assert_eq!(bucket(4096, 10), Some("normal"));
        assert_eq!(bucket(102400, 10), Some("normal"));
        assert_eq!(bucket(102401, 10), None);
        assert_eq!(bucket(1024 * 1024, 10), None);
        assert_eq!(bucket(1024 * 1024 + 1, 10), Some("large"));
        assert_eq!(bucket(10, 9), Some("tiny"));
    }
    #[test]
    fn novelty_includes_threshold_crossings_and_missing_nodes() {
        let mut baseline = Features::default();
        baseline.edges.insert((1, 2, 3), 10);
        let mut candidate = baseline.clone();
        assert!(!candidate.adds_to(&baseline));
        candidate.edges.insert((1, 2, 3), 11);
        assert!(candidate.adds_to(&baseline));
        baseline.merge(&candidate);
        assert!(!candidate.adds_to(&baseline));
        candidate.missing.insert(5);
        assert!(candidate.adds_to(&baseline));
    }
}
