use super::*;
use serde::ser::SerializeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

struct Cancellation {
    requested: Arc<AtomicBool>,
    handlers: Vec<signal_hook::SigId>,
}

impl Cancellation {
    fn install() -> Result<Self> {
        let mut cancellation = Self {
            requested: Arc::new(AtomicBool::new(false)),
            handlers: Vec::new(),
        };
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            cancellation.handlers.push(signal_hook::flag::register(
                signal,
                Arc::clone(&cancellation.requested),
            )?);
        }
        Ok(cancellation)
    }
}

impl Drop for Cancellation {
    fn drop(&mut self) {
        for &handler in &self.handlers {
            signal_hook::low_level::unregister(handler);
        }
    }
}

#[derive(Deserialize)]
struct Source {
    path: String,
    sha256: String,
    weights: BTreeMap<String, u64>,
}

#[derive(Serialize)]
struct Aggregate {
    files: u64,
    source_bytes: u64,
    nodes: u64,
    error_nodes: u64,
    configurations: Vec<Evaluation>,
    invalid_file_counts: Vec<u64>,
}

fn aggregate(configurations: &[Configuration]) -> Aggregate {
    Aggregate {
        files: 0,
        source_bytes: 0,
        nodes: 0,
        error_nodes: 0,
        configurations: configurations
            .iter()
            .map(|configuration| Evaluation {
                configuration_id: configuration_id(configuration),
                configuration: configuration.clone(),
                totals: Metrics::default(),
                invalid_files: Vec::new(),
                pareto: false,
            })
            .collect(),
        invalid_file_counts: vec![0; configurations.len()],
    }
}

fn scale(metrics: &Metrics, weight: u64) -> Metrics {
    let mut result = metrics.clone();
    macro_rules! scale {
        ($($field:ident),* $(,)?) => { $(result.$field *= weight;)* };
    }
    scale!(
        total_bytes,
        file_header_bytes,
        group_header_bytes,
        occupied_inline_bits,
        overflow_waste_bits,
        final_waste_bits,
        padding_bits,
        lane_waste_bits,
        word_tail_waste_bits,
        nodes,
        groups,
    );
    for count in result.occupancy.values_mut() {
        *count *= weight;
    }
    for count in result.overflow_triggers.values_mut() {
        *count *= weight;
    }
    result
}

pub(super) fn run(arguments: &[String]) -> Result<bool> {
    ensure!(
        arguments.len() == 6,
        "run LIBRARY SYMBOL GRAMMAR SOURCES_JSONL SEARCH OUTPUT"
    );
    let code_corpora_sha = code_corpora_sha()?;
    let cancellation = Cancellation::install()?;
    let search: Search = read_json(&arguments[4])?;
    let library_hash = file_digest(&arguments[0])?;
    let library = unsafe { libloading::Library::new(&arguments[0]) }?;
    let function =
        unsafe { library.get::<unsafe extern "C" fn() -> *const ()>(arguments[1].as_bytes()) }?;
    let language = Language::new(unsafe { LanguageFn::from_raw(*function) });
    let bounds = [
        language.node_kind_count() as u64,
        language.node_kind_count() as u64,
        language.field_count() as u64,
        31,
    ];
    let configurations = configurations(&search, &bounds)?;
    let mut scopes: BTreeMap<String, Aggregate> = BTreeMap::new();
    let output_path = &arguments[5];
    let mut ledger = writer(&format!("{output_path}.files.jsonl"))?;
    let mut processed = 0u64;
    let mut failed = 0u64;
    let mut failed_occurrences = 0u64;
    let mut physical_files = 0u64;
    let mut total_nodes = 0u64;
    let mut planned_unique = 0u64;
    let mut planned_physical_files = 0u64;
    let mut deferred_unique = 0u64;
    let mut deferred_occurrences = 0u64;
    let started = Instant::now();
    for line in BufReader::new(File::open(&arguments[3])?).lines() {
        let source: Source = serde_json::from_str(&line?)?;
        let weight: u64 = source.weights.values().sum();
        ensure!(
            weight > 0 && !source.weights.contains_key("all"),
            "invalid occurrence weights"
        );
        planned_unique += 1;
        planned_physical_files += weight;
        if cancellation.requested.load(Ordering::Relaxed) {
            deferred_unique += 1;
            deferred_occurrences += weight;
            serde_json::to_writer(
                &mut ledger,
                &serde_json::json!({
                    "path": source.path, "sha256": source.sha256, "weights": source.weights,
                    "status": "deferred", "reason": "cancelled",
                }),
            )?;
            writeln!(ledger)?;
            continue;
        }
        let file = (|| -> Result<Extracted> {
            let bytes = fs::read(&source.path)?;
            ensure!(
                digest(&bytes) == source.sha256,
                "source changed since inventory"
            );
            let (bounds, nodes) = extract_limited(&bytes, &language, Some(30))?;
            let file = Extracted {
                schema: SCHEMA,
                path: source.path.clone(),
                grammar: arguments[2].clone(),
                grammar_sha256: library_hash.clone(),
                runtime_revision: RUNTIME_REVISION.into(),
                source_sha256: source.sha256.clone(),
                source_bytes: bytes.len() as u64,
                error_nodes: nodes.iter().filter(|record| record[3] & 8 != 0).count() as u64,
                bounds,
                nodes,
            };
            validate(&file)?;
            Ok(file)
        })();
        processed += 1;
        physical_files += weight;
        match file {
            Err(error) => {
                failed += 1;
                failed_occurrences += weight;
                serde_json::to_writer(
                    &mut ledger,
                    &serde_json::json!({
                        "path": source.path, "sha256": source.sha256, "weights": source.weights,
                        "status": "failed", "error": format!("{error:#}"),
                    }),
                )?;
            }
            Ok(file) => {
                total_nodes += file.nodes.len() as u64 * weight;
                let results = configurations
                    .iter()
                    .map(|config| simulate(&file.nodes, config))
                    .collect::<Vec<_>>();
                for (scope, weight) in source
                    .weights
                    .iter()
                    .map(|(scope, &weight)| (scope.as_str(), weight))
                    .chain(std::iter::once(("all", weight)))
                {
                    let aggregate = scopes
                        .entry(scope.to_string())
                        .or_insert_with(|| aggregate(&configurations));
                    aggregate.files += weight;
                    aggregate.source_bytes += file.source_bytes * weight;
                    aggregate.nodes += file.nodes.len() as u64 * weight;
                    aggregate.error_nodes += file.error_nodes * weight;
                    for (index, result) in results.iter().enumerate() {
                        if let Some(metrics) = result {
                            aggregate.configurations[index]
                                .totals
                                .add(&scale(metrics, weight));
                        } else {
                            aggregate.invalid_file_counts[index] += weight;
                            if aggregate.configurations[index].invalid_files.len() < 10 {
                                aggregate.configurations[index]
                                    .invalid_files
                                    .push(source.path.clone());
                            }
                        }
                    }
                }
                let best = results
                    .iter()
                    .enumerate()
                    .filter_map(|(index, result)| {
                        result.as_ref().map(|metrics| (metrics.total_bytes, index))
                    })
                    .min();
                serde_json::to_writer(
                    &mut ledger,
                    &serde_json::json!({
                        "path": source.path, "sha256": source.sha256, "weights": source.weights,
                        "status": "ok", "nodes": file.nodes.len(), "source_bytes": file.source_bytes,
                        "error_nodes": file.error_nodes, "per_file_oracle": best,
                    }),
                )?;
            }
        }
        writeln!(ledger)?;
        if processed.is_multiple_of(100) {
            ledger.flush()?;
            let progress = serde_json::json!({"grammar": arguments[2], "code_corpora_sha": code_corpora_sha, "processed_unique": processed,
                "physical_files": physical_files, "failed_unique": failed,
                "nodes": total_nodes, "seconds": started.elapsed().as_secs(), "last_file": source.path});
            fs::write(
                format!("{output_path}.progress.json"),
                serde_json::to_vec(&progress)?,
            )?;
            eprintln!("{progress}");
        }
    }
    for aggregate in scopes.values_mut() {
        mark_frontier(&mut aggregate.configurations);
    }
    ledger.flush()?;
    let cancelled = deferred_unique > 0;
    let mut output = writer(output_path)?;
    let metadata = serde_json::json!({
        "schema": SCHEMA, "model": MODEL, "grammar": arguments[2], "grammar_sha256": library_hash,
        "runtime_revision": RUNTIME_REVISION, "columns": COLUMNS,
        "search_sha256": file_digest(&arguments[4])?, "sources_sha256": file_digest(&arguments[3])?,
        "processed_unique": processed, "physical_files": physical_files,
        "failed_unique": failed, "failed_occurrences": failed_occurrences,
        "complete": failed == 0 && !cancelled, "cancelled": cancelled,
        "input_complete": !cancelled, "planned_unique": planned_unique,
        "planned_physical_files": planned_physical_files,
        "deferred_unique": deferred_unique, "deferred_occurrences": deferred_occurrences,
        "parse_deadline_seconds": 30,
        "configuration_count": configurations.len(), "code_corpora_sha": code_corpora_sha,
        "search_coverage": "exhaustive for supplied domain and successful sources only",
        "seconds": started.elapsed().as_secs(),
    });
    // Stream aggregates directly: turning all scopes into a JSON Value first
    // duplicates the expanded configuration matrix in memory.
    let mut serializer = serde_json::Serializer::new(&mut output);
    let mut report = serde::Serializer::serialize_map(&mut serializer, None)?;
    for (key, value) in metadata.as_object().expect("report metadata") {
        report.serialize_entry(key, value)?;
    }
    report.serialize_entry("scopes", &scopes)?;
    report.end()?;
    writeln!(output)?;
    output.flush()?;
    Ok(cancelled)
}
