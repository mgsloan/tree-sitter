mod compare;
mod measure;
mod queries;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use corpus_analysis::{
    Input, LoadedGrammar, Random, Registry, digest, inventory, mutate, parse, read_sampling,
    seed_for, select,
};
use measure::{Meter, Metrics, percentile};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use tree_sitter::Point;
use tree_sitter_squatter::{PackOptions, Tree};

const BENCHMARKS: &[&str] = &[
    "query-matches",
    "query-captures",
    "walk-forward",
    "cursor-forward",
    "iterator-forward",
    "iterator-forward-cached",
    "walk-iterator",
    "walk-iterator-cached",
    "seek-byte",
    #[cfg(feature = "points")]
    "seek-point",
    "cold-parse",
];
const PERCENTILES: [f64; 6] = [0.0, 50.0, 90.0, 95.0, 99.0, 100.0];

#[derive(Parser, Serialize)]
#[command(about = "Paired mainline/squat query and traversal comparisons and benchmarks")]
struct Arguments {
    /// Benchmark names, sampling names, or source paths.
    selectors: Vec<String>,
    #[arg(long, default_value = "../../code-corpora")]
    code_corpora: PathBuf,
    #[arg(long, default_value = "samplings")]
    samplings: PathBuf,
    #[arg(long)]
    registry: Option<PathBuf>,
    #[arg(long, default_value = "/opt/corpus/grammars")]
    grammar_root: PathBuf,
    #[arg(long)]
    all: bool,
    #[arg(long)]
    count: Option<usize>,
    #[arg(long, default_value_t = 3)]
    repeat: usize,
    #[arg(long)]
    mutate: bool,
    #[arg(long, default_value_t = 42)]
    seed: u64,
    #[arg(long)]
    short_circuit: bool,
    #[arg(long, default_value = "run")]
    output: String,
    #[arg(long, default_value = "bench-outputs")]
    output_directory: PathBuf,
    #[arg(long, default_value_t = 8)]
    batch_size: usize,
    #[arg(long)]
    repo: Vec<String>,
    #[arg(long, default_value_t = 16 * 1024 * 1024)]
    max_file_bytes: u64,
    #[arg(long)]
    repack: bool,
    /// Investigate known mainline seek differences; otherwise count and ignore them.
    #[arg(long)]
    strict_seeks: bool,
    /// Disable squat query scan/plan shortcuts for an ablation run.
    #[arg(long)]
    unoptimized_query: bool,
}

struct Source {
    input: Input,
    bytes: Vec<u8>,
    original_sha256: String,
    tested_sha256: String,
}
struct Pair<'source> {
    source: &'source Source,
    mainline: tree_sitter::Tree,
    squat: Tree,
    mainline_ids: compare::Identities,
    squat_ids: compare::Identities,
    seek_bytes: Vec<usize>,
    seek_points: Vec<Point>,
}
#[derive(Serialize)]
struct FileResult {
    path: String,
    grammar: String,
    benchmark: String,
    source_sha256: String,
    tested_sha256: String,
    source_bytes: usize,
    nodes: usize,
    slab_bytes: usize,
    groups: u32,
    group_capacity: u32,
    repeats: usize,
    mainline: Metrics,
    squat: Metrics,
    ratios: BTreeMap<&'static str, Option<f64>>,
    failures: usize,
    ignored_differences: usize,
    expected_field_differences: usize,
}
struct Accumulated {
    result: FileResult,
    mainline: Vec<Metrics>,
    squat: Vec<Metrics>,
}
#[derive(Default, Serialize)]
struct Failures {
    count: usize,
    first: Option<serde_json::Value>,
}
impl Failures {
    fn record(&mut self, path: &str, benchmark: &str, message: impl std::fmt::Display) {
        if self.first.is_none() {
            self.first = Some(
                serde_json::json!({"path": path, "benchmark": benchmark, "message": message.to_string()}),
            );
            eprintln!("first failure: {path} {benchmark}: {message}");
        }
        self.count += 1;
    }
}

fn choose(
    arguments: &Arguments,
    registry: &Registry,
) -> Result<(Vec<String>, Vec<Input>, serde_json::Value)> {
    let mut benchmarks = Vec::new();
    let mut selections = Vec::new();
    for selector in &arguments.selectors {
        if BENCHMARKS.contains(&selector.as_str()) {
            benchmarks.push(selector.clone());
        } else if selector == "seek-point" {
            bail!("seek-point requires the points Cargo feature");
        } else if selector.starts_with("query-") {
            bail!("unknown query benchmark: {selector}");
        } else {
            selections.push(selector.clone());
        }
    }
    if benchmarks.is_empty() {
        benchmarks = BENCHMARKS.iter().map(|name| (*name).to_owned()).collect();
    }
    benchmarks.sort();
    benchmarks.dedup();
    let mut coverage = serde_json::Value::Null;
    let mut inputs = Vec::new();
    if arguments.all {
        let mut inventory = inventory(
            &arguments.code_corpora,
            registry,
            &arguments.repo,
            arguments.max_file_bytes,
        );
        inputs = std::mem::take(&mut inventory.inputs);
        coverage = serde_json::to_value(inventory)?;
    }
    if selections.is_empty() && !arguments.all {
        selections = vec!["train-tiny".into(), "train-small".into()];
    }
    for selection in selections {
        if !Path::new(&selection).is_absolute()
            && !Path::new(&selection).is_file()
            && arguments.samplings.join(&selection).is_file()
        {
            inputs.extend(select(
                read_sampling(
                    &arguments.code_corpora,
                    &arguments.samplings,
                    &selection,
                    registry,
                )?,
                arguments.count,
                arguments.seed,
                &selection,
            ));
        } else {
            let path = if Path::new(&selection).is_file() {
                PathBuf::from(&selection)
            } else {
                arguments.code_corpora.join(&selection)
            };
            let path = path
                .canonicalize()
                .with_context(|| format!("unknown sampling or path: {selection}"))?;
            let relative = path
                .strip_prefix(arguments.code_corpora.canonicalize()?)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let grammar = registry
                .classify(&path)
                .context("unclassified source path")?;
            ensure!(
                registry.grammars.contains_key(grammar),
                "unavailable grammar: {grammar}"
            );
            inputs.push(Input {
                path: relative,
                grammar: grammar.to_owned(),
                bytes: fs::metadata(path)?.len(),
            });
        }
    }
    inputs.sort_by(|a, b| a.path.cmp(&b.path));
    inputs.dedup_by(|a, b| a.path == b.path);
    ensure!(!inputs.is_empty(), "no inputs selected");
    for input in &inputs {
        ensure!(
            input.bytes <= arguments.max_file_bytes,
            "oversized source: {}",
            input.path
        );
    }
    Ok((
        benchmarks,
        select(
            inputs,
            arguments.count,
            arguments.seed,
            "selected-input-union",
        ),
        coverage,
    ))
}

fn seek_positions(source: &Source, seed: u64) -> (Vec<usize>, Vec<Point>) {
    let mut starts = vec![0usize];
    for (index, &byte) in source.bytes.iter().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    let mut random = Random::new(seed_for(seed, "seek-positions", &source.input.path));
    let bytes: Vec<_> = (0..100)
        .map(|_| random.index(source.bytes.len() + 1))
        .collect();
    let points = bytes
        .iter()
        .map(|&byte| {
            let row = starts.partition_point(|&start| start <= byte) - 1;
            Point::new(row, byte - starts[row])
        })
        .collect();
    (bytes, points)
}

fn accumulate(
    results: &mut BTreeMap<(String, String), Accumulated>,
    source: &Source,
    benchmark: &str,
    mainline: Metrics,
    squat: Metrics,
    tree: &Tree,
    failed: bool,
) {
    let entry = results
        .entry((source.input.path.clone(), benchmark.to_owned()))
        .or_insert_with(|| Accumulated {
            result: FileResult {
                path: source.input.path.clone(),
                grammar: source.input.grammar.clone(),
                benchmark: benchmark.to_owned(),
                source_sha256: source.original_sha256.clone(),
                tested_sha256: source.tested_sha256.clone(),
                source_bytes: source.bytes.len(),
                nodes: tree.root_node().descendant_count(),
                slab_bytes: tree.as_bytes().len(),
                groups: tree.group_count(),
                group_capacity: tree.group_capacity(),
                repeats: 0,
                mainline: Metrics::default(),
                squat: Metrics::default(),
                ratios: BTreeMap::new(),
                failures: 0,
                ignored_differences: 0,
                expected_field_differences: 0,
            },
            mainline: Vec::new(),
            squat: Vec::new(),
        });
    entry.mainline.push(mainline);
    entry.squat.push(squat);
    entry.result.failures += usize::from(failed);
}

#[derive(Debug, Eq, PartialEq)]
enum Observation<'tree> {
    Walk(Vec<compare::Record<'tree>>),
    Seek(Vec<Option<usize>>),
    Navigation(Vec<usize>),
    Query(Vec<queries::Record>),
}
fn observe<'tree, N: tree_sitter_squatter::traits::NodeLike<'tree>>(
    root: N,
    ids: &compare::Identities,
    benchmark: &str,
    bytes: &[usize],
    _points: &[Point],
) -> Result<Observation<'tree>> {
    match benchmark {
        "cursor-forward" | "iterator-forward" | "iterator-forward-cached" => Ok(
            Observation::Navigation(compare::navigate(root.cursor()?, ids)),
        ),
        "walk-forward" | "walk-iterator" | "walk-iterator-cached" => {
            Ok(Observation::Walk(compare::walk(root, ids)?))
        }
        "seek-byte" => Ok(Observation::Seek(compare::seek_bytes(root, ids, bytes))),
        #[cfg(feature = "points")]
        "seek-point" => Ok(Observation::Seek(compare::seek_points(root, ids, _points))),
        _ => unreachable!(),
    }
}
fn difference(expected: &Observation<'_>, actual: &Observation<'_>) -> Option<String> {
    if expected == actual {
        return None;
    }
    match (expected, actual) {
        (Observation::Walk(a), Observation::Walk(b)) => {
            let index = a
                .iter()
                .zip(b)
                .position(|(a, b)| a != b)
                .unwrap_or(a.len().min(b.len()));
            Some(format!(
                "walk item {index}: expected {:?}, actual {:?}; lengths {}/{}",
                a.get(index),
                b.get(index),
                a.len(),
                b.len()
            ))
        }
        (Observation::Navigation(a), Observation::Navigation(b)) => {
            let index = a
                .iter()
                .zip(b)
                .position(|(a, b)| a != b)
                .unwrap_or(a.len().min(b.len()));
            Some(format!(
                "navigation item {index}: expected {:?}, actual {:?}; lengths {}/{}",
                a.get(index),
                b.get(index),
                a.len(),
                b.len()
            ))
        }
        (Observation::Seek(a), Observation::Seek(b)) => {
            let index = a
                .iter()
                .zip(b)
                .position(|(a, b)| a != b)
                .unwrap_or(a.len().min(b.len()));
            Some(format!(
                "seek sample {index}: expected ordinal {:?}, actual {:?}",
                a.get(index),
                b.get(index)
            ))
        }
        (Observation::Query(a), Observation::Query(b)) => {
            let index = a
                .iter()
                .zip(b)
                .position(|(a, b)| a != b)
                .unwrap_or(a.len().min(b.len()));
            Some(format!(
                "query event {index}: expected {:?}, actual {:?}; lengths {}/{}",
                a.get(index),
                b.get(index),
                a.len(),
                b.len()
            ))
        }
        _ => unreachable!(),
    }
}

fn write_summaries(
    path: &Path,
    results: &[FileResult],
    per_language: bool,
    partial: bool,
) -> Result<()> {
    let mut output = BufWriter::new(OpenOptions::new().create_new(true).write(true).open(path)?);
    let mut groups: BTreeMap<(String, String), Vec<&FileResult>> = BTreeMap::new();
    for result in results {
        let language = if per_language {
            result.grammar.clone()
        } else {
            "all".into()
        };
        groups
            .entry((language, result.benchmark.clone()))
            .or_default()
            .push(result);
    }
    for ((language, benchmark), entries) in groups {
        let mut statistics = BTreeMap::new();
        for (index, name) in Metrics::NAMES.iter().enumerate() {
            let mut mainline: Vec<_> = entries
                .iter()
                .filter_map(|result| result.mainline.values()[index])
                .collect();
            let mut squat: Vec<_> = entries
                .iter()
                .filter_map(|result| result.squat.values()[index])
                .collect();
            let mut ratios: Vec<_> = entries
                .iter()
                .filter_map(|result| result.ratios[name])
                .collect();
            mainline.sort_by(f64::total_cmp);
            squat.sort_by(f64::total_cmp);
            ratios.sort_by(f64::total_cmp);
            statistics.insert(
                name,
                serde_json::json!({
                    "mainline": PERCENTILES.map(|percent| percentile(&mainline, percent)),
                    "squat": PERCENTILES.map(|percent| percentile(&squat, percent)),
                    "paired_ratios": PERCENTILES.map(|percent| percentile(&ratios, percent)),
                    "available_pairs": ratios.len(),
                }),
            );
        }
        serde_json::to_writer(
            &mut output,
            &serde_json::json!({"language": language, "benchmark": benchmark,
            "files": entries.len(), "partial": partial, "percentiles": PERCENTILES, "statistics": statistics}),
        )?;
        writeln!(output)?;
    }
    output.flush()?;
    Ok(())
}

fn git_identity(directory: &Path) -> serde_json::Value {
    let revision = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(directory)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(directory)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty());
    serde_json::json!({"revision": revision, "dirty": dirty})
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    ensure!(
        arguments.repeat > 0 && arguments.batch_size > 0,
        "repeat and batch size must be positive"
    );
    ensure!(
        Path::new(&arguments.output).components().count() == 1
            && !arguments.output.starts_with('.'),
        "--output must be a simple name"
    );
    let registry = if let Some(path) = &arguments.registry {
        Registry::read(path)?
    } else {
        Registry::from_artifacts(&arguments.grammar_root)?
    };
    let (benchmarks, inputs, coverage) = choose(&arguments, &registry)?;
    ensure!(!inputs.is_empty(), "--count selected no files");
    fs::create_dir_all(&arguments.output_directory)?;
    let prefix = arguments.output_directory.join(&arguments.output);
    let output_path = |suffix: &str| PathBuf::from(format!("{}-{suffix}", prefix.display()));
    for suffix in [
        "files.jsonl",
        "languages.jsonl",
        "aggregate.jsonl",
        "run.json",
    ] {
        ensure!(
            !output_path(suffix).exists(),
            "output exists: {}",
            output_path(suffix).display()
        );
    }
    let mut file_output = BufWriter::new(
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(output_path("files.jsonl"))?,
    );
    let mut meter = Meter::new();
    let mut manifest = serde_json::json!({
        "schema": 1, "point_positions": tree_sitter_squatter::HAS_POINT_POSITIONS, "arguments": arguments, "benchmarks": benchmarks, "seed": arguments.seed,
        "inputs": inputs, "planned": inputs.len(), "completed": 0, "failed": 0, "partial": true,
        "coverage": coverage, "registry": registry, "counter_status": meter.counter_status,
        "tool": {"checkout": git_identity(Path::new(".")), "container_revision": std::env::var("SQUAT_TOOL_SHA").ok(), "source_sha256": std::env::var("SQUAT_SOURCE_SHA256").ok(),
                 // When explicitly invoked through ld-linux, current_exe points
                 // at the loader. argv[0] still names the benchmark executable.
                 "binary_sha256": std::env::args_os().next().and_then(|path| fs::read(path).ok())
                    .or_else(|| std::env::current_exe().ok().and_then(|path| fs::read(path).ok()))
                    .map(|bytes| digest(&bytes))}, "code_corpora": git_identity(&arguments.code_corpora),
        "machine": {"architecture": std::env::consts::ARCH, "os": std::env::consts::OS,
                    "cpuinfo": fs::read_to_string("/proc/cpuinfo").ok().and_then(|text| text.lines().find(|line| line.starts_with("model name")).map(str::to_owned))},
        "build": {"debug_assertions": cfg!(debug_assertions), "package_version": env!("CARGO_PKG_VERSION")},
        "field_contract": "field API differences expected only when squat agrees with mainline visible-child fields; ERROR parents have no fields",
        "iterator_contract": "native preorder; walks read O(1) bulk attributes; cached attribute walks use the unpack cache; navigation-only caches are idle; mainline uses its forward cursor",
        "cursor_contract": "walk-forward reads O(1) bulk attributes, excluding counts, fields, and depth from the Rust snapshot; cursor-forward measures native navigation",
        "workload_order": "rotate by batch and every two repeats, retaining both backend orders for each rotation",
        "query_engine": "slab NFA and structural plans adapted from ../main", "seek_contract": if arguments.strict_seeks { "strict" } else { "known differences counted but ignored by user request" },
    });
    fs::write(
        output_path("run.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    let mut grammars = BTreeMap::new();
    let mut queries = BTreeMap::new();
    let wants_queries = benchmarks.iter().any(|name| name.starts_with("query-"));
    let mut failures = Failures::default();
    let mut results = BTreeMap::new();
    let mut completed = BTreeSet::new();
    let mut failed_files = BTreeSet::new();
    let mut ignored_seek_differences = 0usize;
    let mut expected_field_differences = 0usize;
    'batches: for (batch_index, batch) in inputs.chunks(arguments.batch_size).enumerate() {
        let mut sources = Vec::new();
        for input in batch {
            let loaded = (|| -> Result<Source> {
                if !grammars.contains_key(&input.grammar) {
                    grammars.insert(
                        input.grammar.clone(),
                        // The registry identifies trusted grammar exports. This map
                        // outlives every parser/tree/query created below.
                        unsafe { LoadedGrammar::open(&registry.grammars[&input.grammar])? },
                    );
                }
                if wants_queries && !queries.contains_key(&input.grammar) {
                    queries.insert(
                        input.grammar.clone(),
                        queries::Queries::load(
                            &grammars[&input.grammar].language,
                            &registry.grammars[&input.grammar].queries,
                        )?,
                    );
                }
                let original = fs::read(arguments.code_corpora.join(&input.path))?;
                ensure!(
                    original.len() as u64 <= arguments.max_file_bytes,
                    "source grew beyond maximum size"
                );
                let bytes = if arguments.mutate {
                    mutate(&original, arguments.seed, &input.path)
                } else {
                    original.clone()
                };
                Ok(Source {
                    input: input.clone(),
                    original_sha256: digest(&original),
                    tested_sha256: digest(&bytes),
                    bytes,
                })
            })();
            match loaded {
                Ok(source) => sources.push(source),
                Err(error) => {
                    failures.record(&input.path, "load", format!("{error:#}"));
                    failed_files.insert(input.path.clone());
                    if arguments.short_circuit {
                        break 'batches;
                    }
                }
            }
        }
        for repeat in 0..arguments.repeat {
            let mut pairs = Vec::new();
            for source in &sources {
                let language = &grammars[&source.input.grammar].language;
                let parse_mainline = || -> Result<_> {
                    let mut parser = tree_sitter::Parser::new();
                    parser.set_language(language)?;
                    parse(&mut parser, &source.bytes, Duration::from_secs(30))
                };
                let parse_squat = || -> Result<_> {
                    let parsed = parse_mainline()?;
                    Ok(Tree::pack_with_options(
                        &parsed,
                        PackOptions {
                            repack: arguments.repack,
                            ..Default::default()
                        },
                    )?)
                };
                let ((mainline, mainline_time), (squat, squat_time)) =
                    if (batch_index + repeat) % 2 == 0 {
                        (meter.measure(parse_mainline), meter.measure(parse_squat))
                    } else {
                        let squat = meter.measure(parse_squat);
                        let mainline = meter.measure(parse_mainline);
                        (mainline, squat)
                    };
                match (mainline, squat) {
                    (Ok(mainline), Ok(squat)) => {
                        let ids = (
                            compare::identities(mainline.root_node()),
                            compare::identities(squat.root_node()),
                        );
                        let (mainline_ids, squat_ids) = match ids {
                            (Ok(a), Ok(b)) => (a, b),
                            (a, b) => {
                                failures.record(
                                    &source.input.path,
                                    "identity",
                                    format!("{a:?} {b:?}"),
                                );
                                failed_files.insert(source.input.path.clone());
                                if arguments.short_circuit {
                                    break 'batches;
                                } else {
                                    continue;
                                }
                            }
                        };
                        let mut cold_failed = false;
                        let mut expected_fields = 0;
                        if benchmarks.iter().any(|name| name == "cold-parse") {
                            let check = (|| -> Result<()> {
                                let expected = Observation::Walk(compare::walk(
                                    mainline.root_node(),
                                    &mainline_ids,
                                )?);
                                let actual = Observation::Walk(compare::walk(
                                    squat.root_node(),
                                    &squat_ids,
                                )?);
                                if let Some(message) = difference(&expected, &actual) {
                                    bail!("{message}");
                                }
                                compare::relationships(
                                    mainline.root_node(),
                                    squat.root_node(),
                                    &mainline_ids,
                                    &squat_ids,
                                    language,
                                    &mut expected_fields,
                                )
                            })();
                            if let Err(error) = check {
                                failures.record(&source.input.path, "cold-parse", error);
                                cold_failed = true;
                                failed_files.insert(source.input.path.clone());
                            }
                        }
                        accumulate(
                            &mut results,
                            source,
                            "cold-parse",
                            mainline_time,
                            squat_time,
                            &squat,
                            cold_failed,
                        );
                        expected_field_differences += expected_fields;
                        results
                            .get_mut(&(source.input.path.clone(), "cold-parse".to_owned()))
                            .unwrap()
                            .result
                            .expected_field_differences += expected_fields;
                        if cold_failed && arguments.short_circuit {
                            break 'batches;
                        }
                        let (seek_bytes, seek_points) = seek_positions(source, arguments.seed);
                        pairs.push(Pair {
                            source,
                            mainline,
                            squat,
                            mainline_ids,
                            squat_ids,
                            seek_bytes,
                            seek_points,
                        });
                    }
                    (a, b) => {
                        failures.record(
                            &source.input.path,
                            "cold-parse",
                            format!("mainline: {:?}; squat: {:?}", a.err(), b.err()),
                        );
                        failed_files.insert(source.input.path.clone());
                        if arguments.short_circuit {
                            break 'batches;
                        }
                    }
                }
            }
            let workload_count = benchmarks
                .iter()
                .filter(|name| *name != "cold-parse")
                .count();
            // Balance first/last workload positions, which can bias small traversals.
            // Rotate only after both backend orders have been used.
            let first_workload = (batch_index + repeat / 2) % workload_count.max(1);
            for benchmark in benchmarks
                .iter()
                .filter(|name| name.as_str() != "cold-parse")
                .cycle()
                .skip(first_workload)
                .take(workload_count)
            {
                let mut mainline_observations = Vec::new();
                let mut squat_observations = Vec::new();
                // Each backend traverses the entire parsed batch before the other
                // starts. Alternate their order to reduce cache/order bias.
                for pass in 0..2 {
                    let mainline_first = (batch_index + repeat) % 2 == 0;
                    let run_mainline = (pass == 0) == mainline_first;
                    for pair in &pairs {
                        if run_mainline {
                            mainline_observations.push(meter.measure(|| {
                                if benchmark.starts_with("query-") {
                                    return queries[&pair.source.input.grammar]
                                        .mainline(
                                            pair.mainline.root_node(),
                                            &pair.mainline_ids,
                                            &pair.source.bytes,
                                            benchmark == "query-captures",
                                        )
                                        .map(Observation::Query);
                                }
                                observe(
                                    pair.mainline.root_node(),
                                    &pair.mainline_ids,
                                    benchmark,
                                    &pair.seek_bytes,
                                    &pair.seek_points,
                                )
                            }));
                        } else {
                            squat_observations.push(meter.measure(|| {
                                if benchmark.starts_with("query-") {
                                    return queries[&pair.source.input.grammar]
                                        .squat(
                                            pair.squat.root_node(),
                                            &pair.squat_ids,
                                            &pair.source.bytes,
                                            benchmark == "query-captures",
                                            !arguments.unoptimized_query,
                                        )
                                        .map(Observation::Query);
                                }
                                if benchmark.starts_with("walk-iterator") {
                                    return compare::walk_iterator(
                                        pair.squat.root_node(),
                                        &pair.squat_ids,
                                        benchmark.ends_with("-cached"),
                                    )
                                    .map(Observation::Walk);
                                }
                                if benchmark.starts_with("iterator-forward") {
                                    return compare::navigate_iterator(
                                        pair.squat.root_node(),
                                        &pair.squat_ids,
                                        benchmark.ends_with("-cached"),
                                    )
                                    .map(Observation::Navigation);
                                }
                                observe(
                                    pair.squat.root_node(),
                                    &pair.squat_ids,
                                    benchmark,
                                    &pair.seek_bytes,
                                    &pair.seek_points,
                                )
                            }));
                        }
                    }
                }
                for ((pair, (expected, mainline_time)), (actual, squat_time)) in pairs
                    .iter()
                    .zip(mainline_observations)
                    .zip(squat_observations)
                {
                    let message = match (&expected, &actual) {
                        (Ok(expected), Ok(actual)) => difference(expected, actual),
                        (a, b) => Some(format!(
                            "mainline: {:?}; squat: {:?}",
                            a.as_ref().err(),
                            b.as_ref().err()
                        )),
                    };
                    let ignore = !arguments.strict_seeks && benchmark.starts_with("seek-");
                    let ignored = message.is_some() && ignore;
                    let failed = message.is_some() && !ignore;
                    if ignored {
                        ignored_seek_differences += 1;
                    }
                    if let Some(message) = message.filter(|_| !ignore) {
                        failures.record(&pair.source.input.path, benchmark, message);
                        failed_files.insert(pair.source.input.path.clone());
                    }
                    accumulate(
                        &mut results,
                        pair.source,
                        benchmark,
                        mainline_time,
                        squat_time,
                        &pair.squat,
                        failed,
                    );
                    if ignored {
                        results
                            .get_mut(&(pair.source.input.path.clone(), benchmark.clone()))
                            .unwrap()
                            .result
                            .ignored_differences += 1;
                    }
                    if failed && arguments.short_circuit {
                        break 'batches;
                    }
                }
            }
            if repeat + 1 == arguments.repeat {
                for pair in &pairs {
                    completed.insert(pair.source.input.path.clone());
                }
            }
        }
        eprintln!(
            "completed {}/{} files; {} comparison failures",
            completed.len(),
            inputs.len(),
            failures.count
        );
    }
    let mut finalized = Vec::new();
    for (_, mut entry) in results {
        entry.result.mainline = Metrics::median(&entry.mainline);
        entry.result.squat = Metrics::median(&entry.squat);
        entry.result.repeats = entry.mainline.len();
        for (index, name) in Metrics::NAMES.iter().enumerate() {
            let paired: Vec<_> = entry
                .mainline
                .iter()
                .zip(&entry.squat)
                .filter_map(|(a, b)| {
                    let baseline = a.values()[index]?;
                    let candidate = b.values()[index]?;
                    (baseline > 0.0).then_some(candidate / baseline)
                })
                .collect();
            let mut paired = paired;
            paired.sort_by(f64::total_cmp);
            entry.result.ratios.insert(name, percentile(&paired, 50.0));
        }
        serde_json::to_writer(&mut file_output, &entry.result)?;
        writeln!(file_output)?;
        finalized.push(entry.result);
    }
    file_output.flush()?;
    let partial = completed.len() != inputs.len();
    write_summaries(&output_path("languages.jsonl"), &finalized, true, partial)?;
    write_summaries(&output_path("aggregate.jsonl"), &finalized, false, partial)?;
    manifest["completed"] = completed.len().into();
    manifest["failed"] = failed_files.len().into();
    manifest["ignored_seek_differences"] = ignored_seek_differences.into();
    manifest["expected_field_differences"] = expected_field_differences.into();
    manifest["failures"] = serde_json::to_value(&failures)?;
    manifest["partial"] = partial.into();
    manifest["queries"] = serde_json::to_value(
        queries
            .iter()
            .map(|(name, queries)| (name, &queries.reports))
            .collect::<BTreeMap<_, _>>(),
    )?;
    manifest["grammar_sha256"] = serde_json::to_value(
        grammars
            .iter()
            .map(|(name, grammar)| (name, &grammar.sha256))
            .collect::<BTreeMap<_, _>>(),
    )?;
    fs::write(
        output_path("run.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    eprintln!(
        "results: {}-{{files,languages,aggregate}}.jsonl; {} failures",
        prefix.display(),
        failures.count
    );
    ensure!(
        failures.count == 0,
        "comparison failures; see {}",
        output_path("run.json").display()
    );
    Ok(())
}
