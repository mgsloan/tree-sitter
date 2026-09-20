//! Paired query timings with both cores consuming the same compiled grammar and input tree.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use corpus_analysis::{LoadedGrammar, Registry, digest, digest_file};
use std::{fs, hint::black_box, path::PathBuf, time::Instant};

#[derive(Parser, serde::Serialize)]
struct Arguments {
    #[arg(long)]
    registry: PathBuf,
    /// Source files; their suffix selects a grammar from the registry.
    inputs: Vec<PathBuf>,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 7)]
    samples: usize,
    #[arg(long, default_value_t = 20)]
    sample_ms: u64,
    /// Query names from the registry. Empty selects every query.
    #[arg(long)]
    query: Vec<String>,
    #[arg(long, value_parser = ["query-matches", "query-captures"])]
    workload: Vec<String>,
    #[arg(long)]
    unoptimized: bool,
    #[arg(long)]
    no_points: bool,
    #[arg(long)]
    no_presence: bool,
    /// Repeat the C core on both sides to estimate measurement noise.
    #[arg(long)]
    baseline: bool,
}

type Record = (usize, Vec<(u32, u32)>);

// Both loops must remain identical: only the statically selected crate differs.
macro_rules! backend {
    ($module:ident, $core:ident) => {
        mod $module {
            use super::*;
            use $core::{Query, QueryCursor, Tree};

            pub fn snapshot(
                query: &Query,
                tree: &Tree,
                source: &[u8],
                optimized: bool,
            ) -> Vec<Record> {
                let mut cursor = QueryCursor::new();
                cursor.set_timeout(Some(std::time::Duration::from_secs(30)));
                cursor.set_optimized(optimized);
                let mut execution = cursor.execute(query, tree.root_node(), source);
                let mut records = Vec::new();
                while let Some(result) = execution.next_match() {
                    records.push((
                        result.pattern_index,
                        result
                            .captures
                            .iter()
                            .map(|capture| (capture.node.slot(), capture.index))
                            .collect(),
                    ));
                }
                assert!(execution.error().is_none());
                records.sort();
                records
            }

            pub fn run(
                query: &Query,
                tree: &Tree,
                source: &[u8],
                captures: bool,
                optimized: bool,
                iterations: usize,
            ) -> (f64, usize) {
                let start = Instant::now();
                let mut count = 0;
                for _ in 0..iterations {
                    // Include cursor construction and engine allocations, as in
                    // the corpus benchmark. Compiling and packing are setup.
                    let mut cursor = QueryCursor::new();
                    cursor.set_timeout(Some(std::time::Duration::from_secs(30)));
                    cursor.set_optimized(optimized);
                    let mut execution = cursor.execute(query, tree.root_node(), source);
                    if captures {
                        while let Some(result) = execution.next_capture() {
                            black_box(result);
                            count += 1;
                        }
                    } else {
                        while let Some(result) = execution.next_match() {
                            black_box(result);
                            count += 1;
                        }
                    }
                    assert!(execution.error().is_none());
                }
                (start.elapsed().as_secs_f64(), count)
            }
        }
    };
}

backend!(reference, tree_squatter);
backend!(candidate, tree_squatter_rust);

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    ensure!(
        !arguments.inputs.is_empty(),
        "provide at least one source file"
    );
    ensure!(
        arguments.samples > 0 && arguments.sample_ms > 0,
        "samples must be positive"
    );
    ensure!(!arguments.output.exists(), "output already exists");
    let binary_sha256 = digest_file(std::env::current_exe()?)?;
    let registry = Registry::read(&arguments.registry)?;
    let mut rows = Vec::new();

    for path in &arguments.inputs {
        let name = registry
            .classify(path)
            .context("unclassified source path")?;
        let grammar = registry.grammars.get(name).context("unavailable grammar")?;
        // The library outlives both backends and every derived parser/query/tree.
        let loaded = unsafe { LoadedGrammar::open(grammar)? };
        let language = &loaded.language;
        let source = fs::read(path)?;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(language)?;
        let native = parser.parse(&source, None).context("parse failed")?;
        let reference_grammar = tree_squatter::Grammar::new(language)?;
        let candidate_grammar = tree_squatter_rust::Grammar::new(language)?;
        let reference_tree = tree_squatter::Tree::pack_with_options(
            &reference_grammar,
            &native,
            tree_squatter::PackOptions {
                points: !arguments.no_points,
                symbol_presence: !arguments.no_presence,
                ..Default::default()
            },
        )?;
        let candidate_tree = tree_squatter_rust::Tree::pack_with_options(
            &candidate_grammar,
            &native,
            tree_squatter_rust::PackOptions {
                points: !arguments.no_points,
                symbol_presence: !arguments.no_presence,
                ..Default::default()
            },
        )?;
        ensure!(
            reference_tree.as_bytes() == candidate_tree.as_bytes(),
            "slabs differ"
        );

        for query_source in &grammar.queries {
            if !arguments.query.is_empty() && !arguments.query.contains(&query_source.name) {
                continue;
            }
            let text = fs::read_to_string(&query_source.path)?;
            ensure!(
                digest(text.as_bytes()) == query_source.sha256,
                "query checksum differs"
            );
            let reference_query = tree_squatter::Query::new(language, &text);
            let candidate_query = tree_squatter_rust::Query::new(language, &text);
            let (reference_query, candidate_query) = match (reference_query, candidate_query) {
                (Ok(reference), Ok(candidate)) => (reference, candidate),
                (Err(_), Err(_)) => continue,
                _ => anyhow::bail!("query compilation differs: {}", query_source.name),
            };
            ensure!(
                reference::snapshot(
                    &reference_query,
                    &reference_tree,
                    &source,
                    !arguments.unoptimized
                ) == candidate::snapshot(
                    &candidate_query,
                    &candidate_tree,
                    &source,
                    !arguments.unoptimized
                ),
                "matches differ: {} / {}",
                path.display(),
                query_source.name
            );

            for captures in [false, true] {
                let workload = if captures {
                    "query-captures"
                } else {
                    "query-matches"
                };
                if !arguments.workload.is_empty()
                    && !arguments
                        .workload
                        .iter()
                        .any(|selected| selected == workload)
                {
                    continue;
                }
                let optimized = !arguments.unoptimized;
                let run_reference = |iterations| {
                    reference::run(
                        &reference_query,
                        &reference_tree,
                        &source,
                        captures,
                        optimized,
                        iterations,
                    )
                };
                let run_candidate = |iterations| {
                    if arguments.baseline {
                        run_reference(iterations)
                    } else {
                        candidate::run(
                            &candidate_query,
                            &candidate_tree,
                            &source,
                            captures,
                            optimized,
                            iterations,
                        )
                    }
                };
                // Warm both equally. One iteration calibrates a shared batch size;
                // the timed runs alternate order to reduce clock/thermal drift.
                let reference_warm = run_reference(1);
                let candidate_warm = run_candidate(1);
                let iterations = ((arguments.sample_ms as f64 / 1000.0)
                    / reference_warm.0.max(candidate_warm.0).max(1e-9))
                .ceil()
                .max(1.0) as usize;
                let mut reference_samples = Vec::new();
                let mut candidate_samples = Vec::new();
                for sample in 0..arguments.samples {
                    let (reference, candidate) = if sample % 2 == 0 {
                        let reference = run_reference(iterations);
                        (reference, run_candidate(iterations))
                    } else {
                        let candidate = run_candidate(iterations);
                        (run_reference(iterations), candidate)
                    };
                    ensure!(
                        reference.1 == reference_warm.1 * iterations
                            && candidate.1 == candidate_warm.1 * iterations,
                        "unstable result counts"
                    );
                    reference_samples.push(reference.0);
                    candidate_samples.push(candidate.0);
                }
                eprintln!("{} / {} / {workload}", path.display(), query_source.name);
                rows.push(serde_json::json!({
                    "path": path, "source_sha256": digest(&source), "grammar": name,
                    "grammar_sha256": loaded.sha256, "query": query_source.name,
                    "query_sha256": query_source.sha256, "workload": workload,
                    "iterations": iterations, "reference_count": reference_warm.1,
                    "candidate_count": candidate_warm.1,
                    "reference_seconds": reference_samples, "candidate_seconds": candidate_samples,
                }));
            }
        }
    }
    ensure!(!rows.is_empty(), "no queries selected");
    fs::write(
        &arguments.output,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": 1, "arguments": arguments, "results": rows,
        "typed_query_scan": cfg!(feature = "typed-query-scan"),
        "typed_presence_scan": cfg!(feature = "typed-presence-scan"),
        "binary_sha256": binary_sha256,
        "revision": std::process::Command::new("git").args(["rev-parse", "HEAD"])
            .output().ok().map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned()),
        "patch_sha256": std::process::Command::new("git").args(["diff", "HEAD"])
            .output().ok().map(|output| digest(&output.stdout)),
            "timing_contract": "cursor construction, execution, and destruction; compilation and packing excluded",
            "resident": "both cores, grammar owners, packed trees, compiled queries, and mainline tree",
        }))?,
    )?;
    Ok(())
}
