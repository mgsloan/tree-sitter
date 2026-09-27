mod compare;
mod measure;
mod pressure;
mod queries;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use corpus_analysis::{
    Input, LoadedGrammar, Random, Registry, digest, digest_file, inventory, mutate, parse,
    read_sampling, seed_for, select,
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
use tree_squatter::{PackContext, PackOptions, Tree};

pub const BACKEND: &str = "rust";

const BENCHMARKS: &[&str] = &[
    "query-matches",
    "query-captures",
    "cursor-forward",
    "scan-forward",
    "seek-byte",
    "seek-point",
    "cold-parse",
    "warm-parse",
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
    /// Disable squat query scan/plan shortcuts for an ablation run.
    #[arg(long)]
    unoptimized_query: bool,
    /// Cache-residency pressure applied to every timed operation.
    #[arg(long, value_enum, default_value_t)]
    pressure: pressure::Mode,
    /// Pressure working-set bytes; defaults to twice the detected LLC size.
    #[arg(long)]
    pressure_bytes: Option<usize>,
    /// Active share of each 10ms tenant quantum.
    #[arg(long, default_value_t = 10)]
    pressure_duty_percent: u8,
    /// Stable condition name recorded for matrix summarization.
    #[arg(long)]
    pressure_label: Option<String>,
    /// Traversals performed inside each navigation/attribute measurement.
    #[arg(long, default_value_t = 1)]
    traversal_iterations: usize,
    /// Pin the benchmark thread to this Linux CPU.
    #[arg(long)]
    benchmark_cpu: Option<usize>,
    /// Pin the concurrent pressure tenant to this Linux CPU.
    #[arg(long)]
    pressure_cpu: Option<usize>,
}

fn measured<T>(
    enabled: bool,
    meter: &mut Meter,
    pressure: &mut pressure::Pressure,
    operation: impl FnOnce() -> T,
) -> (T, Metrics) {
    if !enabled {
        return (operation(), Metrics::default());
    }
    pressure.before_measurement();
    meter.measure(operation)
}

fn input_batches(
    inputs: &[Input],
    maximum_files: usize,
    source_bytes: Option<usize>,
) -> Vec<&[Input]> {
    let mut result = Vec::new();
    let mut start = 0;
    while start < inputs.len() {
        let mut end = start;
        let mut bytes = 0usize;
        while end < inputs.len()
            && source_bytes.map_or(end - start < maximum_files, |target| bytes < target)
        {
            bytes = bytes.saturating_add(inputs[end].bytes as usize);
            end += 1;
        }
        result.push(&inputs[start..end]);
        start = end;
    }
    result
}

fn pressure_report(pressure: &pressure::Pressure, batches: &[&[Input]]) -> serde_json::Value {
    let mut report = pressure.report();
    report["available_source_bytes"] =
        inputs_bytes(batches.iter().flat_map(|batch| batch.iter())).into();
    if pressure.carousel_bytes().is_some() {
        report["carousel_batch_source_bytes"] = serde_json::json!(
            batches
                .iter()
                .map(|batch| inputs_bytes(batch.iter()))
                .collect::<Vec<_>>()
        );
    }
    report
}

fn inputs_bytes<'a>(inputs: impl Iterator<Item = &'a Input>) -> u64 {
    inputs.map(|input| input.bytes).sum()
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    fn input(path: &str, bytes: u64) -> Input {
        Input {
            path: path.into(),
            grammar: "json".into(),
            bytes,
        }
    }

    #[test]
    fn batches_support_file_counts_and_source_working_sets() {
        let inputs = [input("a", 4), input("b", 7), input("c", 1)];
        assert_eq!(
            input_batches(&inputs, 2, None)
                .iter()
                .map(|batch| batch.len())
                .collect::<Vec<_>>(),
            [2, 1]
        );
        let carousel = input_batches(&inputs, 1, Some(10));
        assert_eq!(
            carousel.iter().map(|batch| batch.len()).collect::<Vec<_>>(),
            [2, 1]
        );
        assert_eq!(
            carousel
                .iter()
                .map(|batch| inputs_bytes(batch.iter()))
                .collect::<Vec<_>>(),
            [11, 1]
        );
    }

    #[test]
    fn summaries_keep_all_cases_and_a_successful_direct_subset() {
        let results: Vec<_> = [
            (FellerStatus::Ok, 1.0),
            (FellerStatus::Ok, 3.0),
            (FellerStatus::UnsupportedGrammar, 100.0),
            (FellerStatus::MainlineSyntaxError, 1000.0),
            (FellerStatus::Failed, 10000.0),
        ]
        .into_iter()
        .map(|(status, wall_ms)| {
            let mainline = Metrics {
                wall_ms,
                ..Metrics::default()
            };
            let squat = Metrics {
                wall_ms: wall_ms * wall_ms,
                ..Metrics::default()
            };
            let feller = Metrics {
                wall_ms: wall_ms * 0.5,
                ..Metrics::default()
            };
            FileResult {
                path: String::new(),
                grammar: "c".into(),
                benchmark: "warm-parse".into(),
                source_sha256: String::new(),
                tested_sha256: String::new(),
                source_bytes: 0,
                nodes: 0,
                slab_bytes: 0,
                groups: 0,
                group_capacity: 0,
                repeats: 1,
                mainline,
                squat,
                ratios: paired_ratios(&[mainline], &[squat]),
                feller: Some(FellerResult {
                    status,
                    reason: None,
                    metrics: (status == FellerStatus::Ok).then_some(feller),
                    ratios: paired_ratios(&[mainline], &[feller]),
                    pack_ratios: paired_ratios(&[squat], &[feller]),
                }),
                failures: 0,
                expected_field_differences: 0,
            }
        })
        .collect();
        let entries: Vec<_> = results.iter().collect();
        let summary = summarize_group(&entries);
        assert_eq!(summary["files"], 5);
        assert_eq!(
            summary["feller_coverage"],
            serde_json::json!({
                "ok": 2, "unsupported_grammar": 1, "mainline_syntax_error": 1, "failed": 1,
            })
        );
        let all = &summary["statistics"]["wall_ms"];
        assert_eq!(all["available_pairs"], 5);
        assert_eq!(all["mainline"][1], 100.0);
        assert_eq!(all["paired_ratios"][1], 100.0);
        let successful = &summary["feller_successful"];
        assert_eq!(successful["files"], 2);
        let paired = &successful["statistics"]["wall_ms"];
        assert_eq!(paired["available_pairs"], 2);
        assert_eq!(paired["mainline"][1], 2.0);
        assert_eq!(paired["squat"][1], 5.0);
        assert_eq!(paired["paired_ratios"][1], 2.0);
        assert_eq!(paired["feller"][1], 1.0);
        assert_eq!(paired["feller_paired_ratios"][1], 0.5);
        assert_eq!(
            successful["statistics"]["instructions"]["available_pairs"],
            0
        );
        let skipped = summarize_group(&entries[2..]);
        assert_eq!(skipped["files"], 3);
        assert_eq!(skipped["feller_successful"]["files"], 0);
        assert!(skipped["feller_successful"]["statistics"]["wall_ms"]["mainline"][1].is_null());
    }

    #[test]
    fn direct_parse_measurements_validate_and_classify_results() -> Result<()> {
        let language =
            unsafe { tree_sitter::Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) };
        let mut context = ParseContext::new(&language, true)?;
        let mut meter = Meter::new();
        let mut pressure = pressure::Pressure::new(pressure::Mode::None, None, 10, None, None)?;
        for mode in ["cold-parse", "warm-parse"] {
            for rotation in 0..3 {
                let measurements = measure_parses(
                    &mut context,
                    b"int value = 1;",
                    mode,
                    PackOptions::default(),
                    rotation,
                    false,
                    &mut meter,
                    &mut pressure,
                );
                let native = measurements.mainline.0?;
                let packed = measurements.squat.0?;
                let result = validate_feller(&context, &native, &packed, measurements.feller);
                assert_eq!(result.status, FellerStatus::Ok);
                assert!(result.metrics.is_some());
                let rejected = validate_feller(
                    &context,
                    &native,
                    &packed,
                    Some((
                        Err(anyhow::anyhow!("rejected valid input")),
                        Metrics::default(),
                    )),
                );
                assert_eq!(rejected.status, FellerStatus::Failed);
                assert!(rejected.metrics.is_none());
                let different = context.direct(b"int other;", mode, PackOptions::default());
                assert_eq!(
                    validate_feller(
                        &context,
                        &native,
                        &packed,
                        Some((different, Metrics::default()))
                    )
                    .status,
                    FellerStatus::Failed
                );
            }
            let native = context.native(b"int broken = ;", false, false)?;
            let packed = context.packed(b"int broken = ;", mode, PackOptions::default())?;
            let result = validate_feller(&context, &native, &packed, None);
            assert_eq!(result.status, FellerStatus::MainlineSyntaxError);
            assert!(result.metrics.is_none());
        }
        let language = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let mut context = ParseContext::new(&language, true)?;
        let native = context.native(b"{}", false, false)?;
        let packed = context.packed(b"{}", "cold-parse", PackOptions::default())?;
        let result = validate_feller(&context, &native, &packed, None);
        assert_eq!(result.status, FellerStatus::UnsupportedGrammar);
        assert!(result.metrics.is_none());
        Ok(())
    }
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
    mainline_ids: compare::Identities<usize>,
    squat_ids: compare::Identities<tree_squatter::SlotIx>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    feller: Option<FellerResult>,
    failures: usize,
    expected_field_differences: usize,
}
struct Accumulated {
    result: FileResult,
    mainline: Vec<Metrics>,
    squat: Vec<Metrics>,
    feller: Vec<Metrics>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum FellerStatus {
    Ok,
    UnsupportedGrammar,
    MainlineSyntaxError,
    Failed,
}

#[derive(Serialize)]
struct FellerResult {
    status: FellerStatus,
    reason: Option<String>,
    metrics: Option<Metrics>,
    ratios: BTreeMap<&'static str, Option<f64>>,
    pack_ratios: BTreeMap<&'static str, Option<f64>>,
}

struct ParseContext {
    language: tree_squatter::Language,
    mainline: tree_sitter::Parser,
    packing_parser: tree_sitter::Parser,
    pack: PackContext,
    feller: Option<Result<tree_squatter::Parser, tree_squatter::ParseError>>,
}

impl ParseContext {
    fn new(tree_sitter_language: &tree_sitter::Language, direct: bool) -> Result<Self> {
        let language = tree_squatter::Language::new(tree_sitter_language)?;
        let feller = if direct {
            let parser = tree_squatter::Parser::new(&language);
            if let Err(error) = &parser {
                ensure!(error.code == tree_squatter::Error::Language, "{error}");
            }
            Some(parser)
        } else {
            None
        };
        let mut mainline = tree_sitter::Parser::new();
        mainline.set_language(tree_sitter_language)?;
        let mut packing_parser = tree_sitter::Parser::new();
        packing_parser.set_language(tree_sitter_language)?;
        Ok(Self {
            language,
            mainline,
            packing_parser,
            pack: PackContext::new()?,
            feller,
        })
    }

    fn native(&mut self, source: &[u8], warm: bool, packing: bool) -> Result<tree_sitter::Tree> {
        if warm {
            let parser = if packing {
                &mut self.packing_parser
            } else {
                &mut self.mainline
            };
            parse(parser, source, Duration::from_secs(30))
        } else {
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&self.language.tree_sitter_language())?;
            parse(&mut parser, source, Duration::from_secs(30))
        }
    }

    fn packed(&mut self, source: &[u8], mode: &str, options: PackOptions) -> Result<Tree> {
        let native = self.native(source, mode == "warm-parse", true)?;
        if mode == "cold-parse" {
            let language = tree_squatter::Language::new(&self.language.tree_sitter_language())?;
            Ok(Tree::pack_with_options(&language, &native, options)?)
        } else {
            Ok(self
                .pack
                .pack_with_options(&self.language, &native, options)?)
        }
    }

    fn direct(&mut self, source: &[u8], mode: &str, options: PackOptions) -> Result<Tree> {
        if mode == "cold-parse" {
            let language = tree_squatter::Language::new(&self.language.tree_sitter_language())?;
            Ok(Tree::parse_direct_with_options(&language, source, options)?)
        } else {
            Ok(self
                .feller
                .as_mut()
                .unwrap()
                .as_mut()
                .unwrap()
                .parse_with_options(source, options)?)
        }
    }
}

struct ParseMeasurements {
    mainline: (Result<tree_sitter::Tree>, Metrics),
    squat: (Result<Tree>, Metrics),
    feller: Option<(Result<Tree>, Metrics)>,
}

fn measure_parses(
    context: &mut ParseContext,
    source: &[u8],
    mode: &str,
    options: PackOptions,
    rotation: usize,
    enabled: bool,
    meter: &mut Meter,
    pressure: &mut pressure::Pressure,
) -> ParseMeasurements {
    let direct = mode != "setup-parse" && context.feller.as_ref().is_some_and(Result::is_ok);
    let count = if direct { 3 } else { 2 };
    let (mut mainline, mut squat, mut feller) = (None, None, None);
    for offset in 0..count {
        match (rotation + offset) % count {
            0 => {
                mainline = Some(measured(enabled, meter, pressure, || {
                    context.native(source, mode == "warm-parse", false)
                }))
            }
            1 => {
                squat = Some(measured(enabled, meter, pressure, || {
                    context.packed(source, mode, options)
                }))
            }
            2 => {
                feller = Some(measured(enabled, meter, pressure, || {
                    context.direct(source, mode, options)
                }))
            }
            _ => unreachable!(),
        }
    }
    ParseMeasurements {
        mainline: mainline.unwrap(),
        squat: squat.unwrap(),
        feller,
    }
}

fn validate_feller(
    context: &ParseContext,
    mainline: &tree_sitter::Tree,
    squat: &Tree,
    measured: Option<(Result<Tree>, Metrics)>,
) -> FellerResult {
    let (status, reason, metrics) = if let Some(Err(error)) = &context.feller {
        (
            FellerStatus::UnsupportedGrammar,
            Some(error.to_string()),
            None,
        )
    } else if mainline.root_node().has_error() {
        (
            FellerStatus::MainlineSyntaxError,
            Some("mainline parse requires error recovery".into()),
            None,
        )
    } else {
        let (tree, metrics) = measured.expect("supported direct parser was measured");
        let checked = tree.and_then(|tree| {
            // Capacity estimates may differ; compact serialization preserves every
            // encoded attribute and topology while removing spare allocation space.
            let mut expected = vec![std::mem::MaybeUninit::uninit(); squat.compact_size()];
            let mut actual = vec![std::mem::MaybeUninit::uninit(); tree.compact_size()];
            ensure!(
                squat.copy_compact_into(&mut expected)? == tree.copy_compact_into(&mut actual)?,
                "direct parser produced different packed bytes"
            );
            Ok(())
        });
        match checked {
            Ok(()) => (FellerStatus::Ok, None, Some(metrics)),
            Err(error) => (FellerStatus::Failed, Some(format!("{error:#}")), None),
        }
    };
    FellerResult {
        status,
        reason,
        metrics,
        ratios: BTreeMap::new(),
        pack_ratios: BTreeMap::new(),
    }
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
    feller: Option<FellerResult>,
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
                feller: None,
                failures: 0,
                expected_field_differences: 0,
            },
            mainline: Vec::new(),
            squat: Vec::new(),
            feller: Vec::new(),
        });
    entry.mainline.push(mainline);
    entry.squat.push(squat);
    entry.result.failures += usize::from(failed);
    if let Some(feller) = feller {
        if let Some(metrics) = feller.metrics {
            entry.feller.push(metrics);
        }
        if !entry
            .result
            .feller
            .as_ref()
            .is_some_and(|previous| previous.status == FellerStatus::Failed)
        {
            entry.result.feller = Some(feller);
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Observation<'tree> {
    Walk(Vec<compare::Record<'tree>>),
    Seek(Vec<Option<usize>>),
    Query(Vec<queries::Record>),
}
fn observe<'tree, N: tree_squatter::traits::NodeLike<'tree>>(
    root: N,
    ids: &compare::Identities<N::Id>,
    benchmark: &str,
    bytes: &[usize],
    points: &[Point],
) -> Result<Observation<'tree>> {
    Ok(match benchmark {
        "cursor-forward" | "scan-forward" => Observation::Walk(compare::walk(root, ids)?),
        "seek-byte" => Observation::Seek(compare::seek_bytes(root, ids, bytes)),
        "seek-point" => Observation::Seek(compare::seek_points(root, ids, points)),
        _ => unreachable!(),
    })
}

fn read_nodes<'tree, N: tree_squatter::traits::NodeLike<'tree>>(
    root: N,
    benchmark: &str,
    bytes: &[usize],
    points: &[Point],
    iterations: usize,
) -> Result<usize> {
    match benchmark {
        "cursor-forward" => compare::scan::<_, false>(root, iterations),
        "scan-forward" => compare::scan::<_, true>(root, iterations),
        "seek-byte" => {
            for &byte in bytes {
                std::hint::black_box(root.descendant_for_byte_range(byte, byte));
            }
            Ok(bytes.len())
        }
        "seek-point" => {
            for &point in points {
                std::hint::black_box(root.descendant_for_point_range(point, point));
            }
            Ok(points.len())
        }
        _ => unreachable!(),
    }
}

// Timed reads do not consult identity maps or build correctness snapshots.
fn read_workload(
    pair: &Pair<'_>,
    queries: &BTreeMap<String, queries::Queries>,
    benchmark: &str,
    mainline: bool,
    iterations: usize,
    optimized: bool,
) -> Result<usize> {
    if benchmark.starts_with("query-") {
        let queries = &queries[&pair.source.input.grammar];
        if mainline {
            queries.mainline(
                pair.mainline.root_node(),
                None,
                &pair.source.bytes,
                benchmark == "query-captures",
            )?;
        } else {
            queries.squat(
                pair.squat.root_node(),
                None,
                &pair.source.bytes,
                benchmark == "query-captures",
                optimized,
            )?;
        }
        return Ok(0);
    }
    if mainline {
        read_nodes(
            pair.mainline.root_node(),
            benchmark,
            &pair.seek_bytes,
            &pair.seek_points,
            iterations,
        )
    } else {
        read_nodes(
            pair.squat.root_node(),
            benchmark,
            &pair.seek_bytes,
            &pair.seek_points,
            iterations,
        )
    }
}

fn validate_workload(
    pair: &Pair<'_>,
    queries: &BTreeMap<String, queries::Queries>,
    benchmark: &str,
    optimized: bool,
) -> Result<()> {
    let expected = if benchmark.starts_with("query-") {
        queries[&pair.source.input.grammar]
            .mainline(
                pair.mainline.root_node(),
                Some(&pair.mainline_ids),
                &pair.source.bytes,
                benchmark == "query-captures",
            )
            .map(Observation::Query)
    } else {
        observe(
            pair.mainline.root_node(),
            &pair.mainline_ids,
            benchmark,
            &pair.seek_bytes,
            &pair.seek_points,
        )
    }?;
    let actual = if benchmark.starts_with("query-") {
        queries[&pair.source.input.grammar]
            .squat(
                pair.squat.root_node(),
                Some(&pair.squat_ids),
                &pair.source.bytes,
                benchmark == "query-captures",
                optimized,
            )
            .map(Observation::Query)
    } else {
        observe(
            pair.squat.root_node(),
            &pair.squat_ids,
            benchmark,
            &pair.seek_bytes,
            &pair.seek_points,
        )
    }?;

    if let (Observation::Query(expected), Observation::Query(actual)) = (&expected, &actual) {
        if benchmark == "query-captures" {
            let matches = queries[&pair.source.input.grammar].mainline(
                pair.mainline.root_node(),
                Some(&pair.mainline_ids),
                &pair.source.bytes,
                false,
            )?;
            queries::check_capture_coverage(expected, &matches)?;
            return queries::check_capture_coverage(actual, &matches);
        }
    }
    if let Some(message) = difference(&expected, &actual) {
        bail!("{message}");
    }
    Ok(())
}

fn sequence_difference<T: std::fmt::Debug>(
    label: &str,
    expected: &[T],
    actual: &[T],
    matches: impl Fn(&T, &T) -> bool,
) -> Option<String> {
    let index = expected
        .iter()
        .zip(actual)
        .position(|(expected, actual)| !matches(expected, actual))
        .or_else(|| (expected.len() != actual.len()).then_some(expected.len().min(actual.len())))?;
    Some(format!(
        "{label} {index}: expected {:?}, actual {:?}; lengths {}/{}",
        expected.get(index),
        actual.get(index),
        expected.len(),
        actual.len()
    ))
}

fn difference(expected: &Observation<'_>, actual: &Observation<'_>) -> Option<String> {
    match (expected, actual) {
        (Observation::Walk(expected), Observation::Walk(actual)) => {
            sequence_difference("walk item", expected, actual, |expected, actual| {
                expected.ordinal == actual.ordinal
                    && compare::attributes_match(&expected.attributes, &actual.attributes)
            })
        }
        (Observation::Seek(expected), Observation::Seek(actual)) => {
            sequence_difference("seek sample", expected, actual, PartialEq::eq)
        }
        (Observation::Query(expected), Observation::Query(actual)) => {
            sequence_difference("query event", expected, actual, PartialEq::eq)
        }
        _ => unreachable!(),
    }
}

fn paired_ratios(
    baseline: &[Metrics],
    candidate: &[Metrics],
) -> BTreeMap<&'static str, Option<f64>> {
    Metrics::NAMES
        .iter()
        .enumerate()
        .map(|(index, &name)| {
            let mut pairs: Vec<_> = baseline
                .iter()
                .zip(candidate)
                .filter_map(|(baseline, candidate)| {
                    let baseline = baseline.values()[index]?;
                    let candidate = candidate.values()[index]?;
                    (baseline > 0.0).then_some(candidate / baseline)
                })
                .collect();
            pairs.sort_by(f64::total_cmp);
            (name, percentile(&pairs, 50.0))
        })
        .collect()
}

fn metric_percentiles(values: impl Iterator<Item = f64>) -> [Option<f64>; 6] {
    let mut values: Vec<_> = values.collect();
    values.sort_by(f64::total_cmp);
    PERCENTILES.map(|percent| percentile(&values, percent))
}

fn summary_statistics(entries: &[&FileResult], direct: bool) -> serde_json::Value {
    Metrics::NAMES
        .iter()
        .enumerate()
        .map(|(index, &name)| {
            let ratios: Vec<_> = entries.iter().filter_map(|result| result.ratios[name]).collect();
            let mut statistics = serde_json::json!({
                "mainline": metric_percentiles(entries.iter().filter_map(|result| result.mainline.values()[index])),
                "squat": metric_percentiles(entries.iter().filter_map(|result| result.squat.values()[index])),
                "paired_ratios": metric_percentiles(ratios.iter().copied()),
                "available_pairs": ratios.len(),
            });
            if direct {
                let feller: Vec<_> = entries.iter().map(|result| result.feller.as_ref().unwrap()).collect();
                let ratios: Vec<_> = feller.iter().filter_map(|result| result.ratios[name]).collect();
                let pack_ratios: Vec<_> = feller.iter().filter_map(|result| result.pack_ratios[name]).collect();
                statistics["feller"] = serde_json::json!(metric_percentiles(feller.iter().filter_map(|result| result.metrics?.values()[index])));
                statistics["feller_paired_ratios"] = serde_json::json!(metric_percentiles(ratios.iter().copied()));
                statistics["feller_pack_ratios"] = serde_json::json!(metric_percentiles(pack_ratios.iter().copied()));
                statistics["feller_available_pairs"] = ratios.len().into();
                statistics["feller_pack_available_pairs"] = pack_ratios.len().into();
            }
            (name.to_owned(), statistics)
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

fn summarize_group(entries: &[&FileResult]) -> serde_json::Value {
    let mut coverage = BTreeMap::new();
    let mut successful = Vec::new();
    for &entry in entries {
        if let Some(feller) = &entry.feller {
            *coverage.entry(feller.status).or_insert(0usize) += 1;
            if feller.status == FellerStatus::Ok {
                successful.push(entry);
            }
        }
    }
    serde_json::json!({
        "files": entries.len(),
        "percentiles": PERCENTILES,
        "statistics": summary_statistics(entries, false),
        "feller_coverage": coverage,
        "feller_successful": {
            "files": successful.len(),
            "statistics": summary_statistics(&successful, true),
        },
    })
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
        let mut summary = summarize_group(&entries);
        summary["language"] = language.into();
        summary["benchmark"] = benchmark.into();
        summary["partial"] = partial.into();
        serde_json::to_writer(&mut output, &summary)?;
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

pub fn run(check_only: bool) -> Result<()> {
    let mut arguments = Arguments::parse();
    if check_only {
        arguments.repeat = 1;
        ensure!(
            matches!(arguments.pressure, pressure::Mode::None),
            "squatter-check does not accept cache pressure"
        );
    }
    ensure!(
        arguments.repeat > 0 && arguments.batch_size > 0 && arguments.traversal_iterations > 0,
        "repeat, batch size, and traversal iterations must be positive"
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
    let mut parse_benchmarks: Vec<_> = benchmarks
        .iter()
        .filter(|name| matches!(name.as_str(), "cold-parse" | "warm-parse"))
        .map(String::as_str)
        .collect();
    let wants_direct = !parse_benchmarks.is_empty();
    if parse_benchmarks.is_empty() {
        parse_benchmarks.push("setup-parse");
    }
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
    let mut pressure = pressure::Pressure::new(
        arguments.pressure,
        arguments.pressure_bytes,
        arguments.pressure_duty_percent,
        arguments.benchmark_cpu,
        arguments.pressure_cpu,
    )?;
    let batches = input_batches(&inputs, arguments.batch_size, pressure.carousel_bytes());
    let mut manifest = serde_json::json!({
        "schema": 4, "purpose": if check_only { "correctness" } else { "benchmark" }, "parse_benchmarks": parse_benchmarks, "arguments": arguments, "benchmarks": benchmarks, "seed": arguments.seed,
        "inputs": inputs, "planned": inputs.len(), "completed": 0, "failed": 0, "partial": true,
        "coverage": coverage, "registry": registry, "counter_status": if check_only { "disabled for correctness" } else { &meter.counter_status },
        "tool": {"checkout": git_identity(Path::new(".")), "container_revision": std::env::var("SQUAT_TOOL_SHA").ok(), "source_sha256": std::env::var("SQUAT_SOURCE_SHA256").ok(),
                 // When explicitly invoked through ld-linux, current_exe points
                 // at the loader. argv[0] still names the benchmark executable.
                 "binary_sha256": std::env::args_os().next().and_then(|path| digest_file(path).ok())
                    .or_else(|| std::env::current_exe().ok().and_then(|path| digest_file(path).ok()))}, "code_corpora": git_identity(&arguments.code_corpora),
        "machine": {"architecture": std::env::consts::ARCH, "os": std::env::consts::OS,
                    "cpuinfo": fs::read_to_string("/proc/cpuinfo").ok().and_then(|text| text.lines().find(|line| line.starts_with("model name")).map(str::to_owned))},
        "build": {"debug_assertions": cfg!(debug_assertions), "package_version": env!("CARGO_PKG_VERSION"), "squatter_backend": BACKEND},
        "pressure": pressure_report(&pressure, &batches),
        "field_contract": "field API differences expected only when squat agrees with mainline visible-child fields; ERROR parents have no fields",
        "timing_contract": "v4: cold-parse includes fresh parser and grammar preparation; warm-parse reuses independent parsers and scratch after one untimed warmup per source; direct output validated by compact slab equality; exact validation and snapshots outside timing; read kernels consume results with black_box; no identity lookups or result collections in timed reads",
        "cursor_contract": "scan-forward reads O(1) bulk attributes; cursor-forward measures navigation",
        "feller_contract": "no recovery or fallback; unsupported grammars and mainline syntax errors have null metrics and are excluded from ratios; rejection or differing compact bytes on valid supported inputs fails the run",
        "summary_contract": "statistics includes all cases; feller_successful restricts all three backends to cases where direct parsing and compact slab validation succeeded on every repeat",
        "parse_order": "rotate parse workloads and eligible backends by batch and repeat",
        "workload_order": "rotate by batch and every two repeats, retaining both backend orders for each rotation",
        "query_engine": "slab NFA and structural plans adapted from ../main", "seek_contract": "strict",
        "query_contract": "exact completed matches; captures cover completed captures, with event order, provisional snapshots, and duplicates allowed to differ; coverage checked outside timing",
    });
    fs::write(
        output_path("run.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    let mut grammars = BTreeMap::new();
    // Declared after grammar libraries so contexts drop before their libraries.
    let mut parse_contexts = BTreeMap::new();
    let mut queries = BTreeMap::new();
    let wants_queries = benchmarks.iter().any(|name| name.starts_with("query-"));
    let mut failures = Failures::default();
    let mut results = BTreeMap::new();
    let mut completed = BTreeSet::new();
    let mut failed_files = BTreeSet::new();
    let mut expected_field_differences = 0usize;
    'batches: for (batch_index, batch) in batches.into_iter().enumerate() {
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
                if !parse_contexts.contains_key(&input.grammar) {
                    parse_contexts.insert(
                        input.grammar.clone(),
                        ParseContext::new(&grammars[&input.grammar].language, wants_direct)?,
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
                let context = parse_contexts.get_mut(&source.input.grammar).unwrap();
                let options = PackOptions {
                    repack: arguments.repack,
                    ..Default::default()
                };
                let mut pair = None;
                for &parse_benchmark in parse_benchmarks
                    .iter()
                    .cycle()
                    .skip((batch_index + repeat) % parse_benchmarks.len())
                    .take(parse_benchmarks.len())
                {
                    if parse_benchmark == "warm-parse" && repeat == 0 {
                        drop(measure_parses(
                            context,
                            &source.bytes,
                            parse_benchmark,
                            options,
                            0,
                            false,
                            &mut meter,
                            &mut pressure,
                        ));
                    }
                    let ParseMeasurements {
                        mainline: (mainline, mainline_time),
                        squat: (squat, squat_time),
                        feller,
                    } = measure_parses(
                        context,
                        &source.bytes,
                        parse_benchmark,
                        options,
                        batch_index + repeat,
                        !check_only,
                        &mut meter,
                        &mut pressure,
                    );
                    match (mainline, squat) {
                        (Ok(mainline), Ok(squat)) => {
                            let (mainline_ids, squat_ids) = match (
                                compare::identities(mainline.root_node()),
                                compare::identities(squat.root_node()),
                            ) {
                                (Ok(mainline), Ok(squat)) => (mainline, squat),
                                (mainline, squat) => {
                                    failures.record(
                                        &source.input.path,
                                        "identity",
                                        format!("{mainline:?} {squat:?}"),
                                    );
                                    failed_files.insert(source.input.path.clone());
                                    if arguments.short_circuit {
                                        break 'batches;
                                    } else {
                                        continue;
                                    }
                                }
                            };
                            let mut parse_failed = false;
                            let mut expected_fields = 0;
                            if parse_benchmark != "setup-parse" {
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
                                    failures.record(&source.input.path, parse_benchmark, error);
                                    parse_failed = true;
                                    failed_files.insert(source.input.path.clone());
                                }
                            }
                            let feller = (parse_benchmark != "setup-parse")
                                .then(|| validate_feller(context, &mainline, &squat, feller));
                            if let Some(result) = &feller {
                                if result.status == FellerStatus::Failed {
                                    failures.record(
                                        &source.input.path,
                                        parse_benchmark,
                                        result.reason.as_deref().unwrap(),
                                    );
                                    parse_failed = true;
                                    failed_files.insert(source.input.path.clone());
                                }
                            }
                            accumulate(
                                &mut results,
                                source,
                                parse_benchmark,
                                mainline_time,
                                squat_time,
                                &squat,
                                parse_failed,
                                feller,
                            );
                            expected_field_differences += expected_fields;
                            results
                                .get_mut(&(source.input.path.clone(), parse_benchmark.to_owned()))
                                .unwrap()
                                .result
                                .expected_field_differences += expected_fields;
                            if parse_failed && arguments.short_circuit {
                                break 'batches;
                            }
                            let (seek_bytes, seek_points) = seek_positions(source, arguments.seed);
                            pair = Some(Pair {
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
                                parse_benchmark,
                                format!("mainline: {:?}; squat: {:?}", a.err(), b.err()),
                            );
                            failed_files.insert(source.input.path.clone());
                            if arguments.short_circuit {
                                break 'batches;
                            }
                        }
                    }
                }
                if let Some(pair) = pair {
                    pairs.push(pair);
                }
            }
            let workload_count = benchmarks
                .iter()
                .filter(|name| !matches!(name.as_str(), "cold-parse" | "warm-parse"))
                .count();
            // Balance first/last workload positions, which can bias small traversals.
            // Rotate only after both backend orders have been used.
            let first_workload = (batch_index + repeat / 2) % workload_count.max(1);
            for benchmark in benchmarks
                .iter()
                .filter(|name| !matches!(name.as_str(), "cold-parse" | "warm-parse"))
                .cycle()
                .skip(first_workload)
                .take(workload_count)
            {
                // Release correctness snapshots before either timed pass. Validation
                // must not interleave with a carousel pass and change its resident set.
                let mut messages: Vec<_> = pairs
                    .iter()
                    .map(|pair| {
                        validate_workload(pair, &queries, benchmark, !arguments.unoptimized_query)
                            .err()
                            .map(|error| error.to_string())
                    })
                    .collect();
                let mut times = vec![[Metrics::default(); 2]; pairs.len()];
                if !check_only {
                    for pass in 0..2 {
                        let mainline = ((batch_index + repeat) % 2 == 0) == (pass == 0);
                        for (index, pair) in pairs.iter().enumerate() {
                            let (result, time) = measured(true, &mut meter, &mut pressure, || {
                                read_workload(
                                    pair,
                                    &queries,
                                    benchmark,
                                    mainline,
                                    arguments.traversal_iterations,
                                    !arguments.unoptimized_query,
                                )
                            });
                            times[index][usize::from(!mainline)] = time;
                            if let Err(error) = result.and_then(|count| {
                                if !benchmark.starts_with("query-")
                                    && !benchmark.starts_with("seek-")
                                {
                                    ensure!(
                                        count
                                            == pair.squat_ids.len()
                                                * arguments.traversal_iterations,
                                        "timed traversal count differs"
                                    );
                                }
                                Ok(())
                            }) {
                                messages[index].get_or_insert_with(|| error.to_string());
                            }
                        }
                    }
                }
                for ((pair, message), [mainline_time, squat_time]) in
                    pairs.iter().zip(messages).zip(times)
                {
                    let failed = message.is_some();
                    if let Some(message) = message {
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
                        None,
                    );
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
        entry.result.ratios = paired_ratios(&entry.mainline, &entry.squat);
        if let Some(feller) = &mut entry.result.feller {
            if feller.status == FellerStatus::Ok {
                assert_eq!(entry.feller.len(), entry.mainline.len());
                feller.metrics = Some(Metrics::median(&entry.feller));
                feller.ratios = paired_ratios(&entry.mainline, &entry.feller);
                feller.pack_ratios = paired_ratios(&entry.squat, &entry.feller);
            }
        }
        serde_json::to_writer(&mut file_output, &entry.result)?;
        writeln!(file_output)?;
        finalized.push(entry.result);
    }
    file_output.flush()?;
    let partial = completed.len() != inputs.len();
    write_summaries(&output_path("languages.jsonl"), &finalized, true, partial)?;
    write_summaries(&output_path("aggregate.jsonl"), &finalized, false, partial)?;
    let mut feller_coverage: BTreeMap<&str, BTreeMap<FellerStatus, usize>> = BTreeMap::new();
    for result in &finalized {
        if let Some(feller) = &result.feller {
            *feller_coverage
                .entry(&result.benchmark)
                .or_default()
                .entry(feller.status)
                .or_default() += 1;
        }
    }
    manifest["feller_coverage"] = serde_json::to_value(feller_coverage)?;
    manifest["completed"] = completed.len().into();
    manifest["failed"] = failed_files.len().into();
    manifest["expected_field_differences"] = expected_field_differences.into();
    manifest["failures"] = serde_json::to_value(&failures)?;
    manifest["partial"] = partial.into();
    manifest["queries"] = serde_json::to_value(
        queries
            .iter()
            .map(|(name, queries)| (name, &queries.reports))
            .collect::<BTreeMap<_, _>>(),
    )?;
    manifest["pressure"] = pressure_report(
        &pressure,
        &input_batches(&inputs, arguments.batch_size, pressure.carousel_bytes()),
    );
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
