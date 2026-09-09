use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use tree_sitter::{Language, Parser};
use tree_sitter_language::LanguageFn;

const COLUMNS: [&str; 11] = [
    "symbol",
    "grammar_symbol",
    "field",
    "flags",
    "subtree_size",
    "start_byte",
    "byte_length",
    "start_row",
    "row_span",
    "start_column",
    "column_length",
];
const GRAMMAR_COLUMNS: usize = 4;
const SCHEMA: u32 = 2;
const MODEL: &str = "squat-swar";
const RUNTIME_REVISION: &str = "072f68c829696687fb01cbc8764e655b3ee942ba";
type Record = [u64; 11];

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Extracted {
    schema: u32,
    path: String,
    grammar: String,
    grammar_sha256: String,
    runtime_revision: String,
    source_sha256: String,
    source_bytes: u64,
    error_nodes: u64,
    bounds: [u64; GRAMMAR_COLUMNS],
    nodes: Vec<Record>,
}

fn code_corpora_sha() -> Result<String> {
    let sha =
        std::env::var("CODE_CORPORA_SHA").context("set CODE_CORPORA_SHA to the corpus commit")?;
    ensure!(
        sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid code-corpora SHA"
    );
    Ok(sha)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn file_digest(path: &str) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

fn extract(source: &[u8], language: &Language) -> Result<([u64; 4], Vec<Record>)> {
    extract_limited(source, language, None)
}

fn extract_limited(
    source: &[u8],
    language: &Language,
    timeout: Option<u64>,
) -> Result<([u64; 4], Vec<Record>)> {
    ensure!(
        source.len() <= u32::MAX as usize,
        "source exceeds tree-sitter's byte range"
    );
    let mut parser = Parser::new();
    parser.set_language(language)?;
    let started = std::time::Instant::now();
    let mut progress = |_: &tree_sitter::ParseState| {
        if timeout.is_some_and(|seconds| started.elapsed().as_secs() >= seconds) {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    };
    let tree = parser
        .parse_with_options(
            &mut |offset, _| source.get(offset..).unwrap_or_default(),
            None,
            Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
        )
        .context("parser returned no tree (parse deadline reached)")?;
    let symbol_count = language.node_kind_count() as u64;
    let encode_symbol = |symbol: u16| {
        if symbol == u16::MAX {
            symbol_count
        } else {
            u64::from(symbol)
        }
    };
    let mut records: Vec<Record> = Vec::new();
    let mut ancestors = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        let start = node.start_position();
        let end = node.end_position();
        let flags = u64::from(node.is_named())
            | (u64::from(node.is_extra()) << 1)
            | (u64::from(node.is_missing()) << 2)
            | (u64::from(node.is_error()) << 3)
            | (u64::from(node.has_error()) << 4);
        ancestors.push(records.len());
        records.push([
            encode_symbol(node.kind_id()),
            encode_symbol(node.grammar_id()),
            cursor.field_id().map_or(0, |field| u64::from(field.get())),
            flags,
            0,
            node.start_byte() as u64,
            (node.end_byte() - node.start_byte()) as u64,
            start.row as u64,
            (end.row - start.row) as u64,
            start.column as u64,
            if end.row == start.row {
                (end.column - start.column) as u64
            } else {
                end.column as u64
            },
        ]);
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            let ancestor = ancestors.pop().expect("cursor ancestry");
            records[ancestor][4] = (records.len() - ancestor) as u64;
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok((
                    [
                        symbol_count,
                        symbol_count,
                        language.field_count() as u64,
                        31,
                    ],
                    records,
                ));
            }
        }
    }
}

/// The only search axis is group capacity. At each capacity evaluate all-u8
/// and each of the seven independent single-field u16 changes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    capacities: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Configuration {
    variant: String,
    capacity: u32,
    widths: [u8; 11],
}

fn configuration_id(configuration: &Configuration) -> String {
    digest(
        &serde_json::to_vec(&(SCHEMA, MODEL, configuration.capacity, configuration.widths))
            .expect("configuration serialization"),
    )
}

fn bits(value: u64) -> u8 {
    (64 - value.leading_zeros()) as u8
}

fn configurations(search: &Search, bounds: &[u64; 4]) -> Result<Vec<Configuration>> {
    ensure!(!search.capacities.is_empty(), "empty capacity sweep");
    let unique: BTreeSet<_> = search.capacities.iter().collect();
    ensure!(
        unique.len() == search.capacities.len(),
        "duplicate capacities"
    );
    ensure!(
        search
            .capacities
            .iter()
            .all(|k| matches!(k, 4 | 8 | 16 | 32 | 64)),
        "capacities must be 4, 8, 16, 32, or 64"
    );
    ensure!(
        bounds.iter().all(|&b| b <= u32::MAX as u64),
        "grammar exceeds u32"
    );
    let names = [
        "all-u8",
        "subtree_size-u16",
        "start_byte-u16",
        "end_byte_sub-u16",
        "start_row-u16",
        "end_row_sub-u16",
        "start_col-u16",
        "end_col_sub-u16",
    ];
    let mut result = Vec::new();
    for (variant, name) in names.into_iter().enumerate() {
        for &capacity in &search.capacities {
            // Reserve two built-in error symbols after the grammar's real IDs.
            let mut widths = [
                bits(bounds[0] + 1).max(2),
                0,
                bits(bounds[2]).max(2),
                5,
                8,
                8,
                8,
                8,
                8,
                8,
                8,
            ];
            if variant > 0 {
                widths[variant + 3] = 16;
            }
            result.push(Configuration {
                variant: name.into(),
                capacity,
                widths,
            });
        }
    }
    Ok(result)
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Metrics {
    total_bytes: u64,
    file_header_bytes: u64,
    group_header_bytes: u64,
    occupied_inline_bits: u64,
    overflow_waste_bits: u64,
    final_waste_bits: u64,
    padding_bits: u64,
    // Subsets of padding_bits; do not add these to the accounting identity again.
    #[serde(default)]
    lane_waste_bits: u64,
    #[serde(default)]
    word_tail_waste_bits: u64,
    nodes: u64,
    groups: u64,
    occupancy: BTreeMap<u32, u64>,
    overflow_triggers: BTreeMap<String, u64>,
}

impl Metrics {
    fn add(&mut self, other: &Self) {
        macro_rules! add {
            ($($field:ident),* $(,)?) => { $(self.$field += other.$field;)* };
        }
        add!(
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
        for (&occupancy, &count) in &other.occupancy {
            *self.occupancy.entry(occupancy).or_default() += count;
        }
        for (trigger, &count) in &other.overflow_triggers {
            *self.overflow_triggers.entry(trigger.clone()).or_default() += count;
        }
    }

    fn verify(&self) {
        assert!(self.lane_waste_bits + self.word_tail_waste_bits <= self.padding_bits);
        assert_eq!(
            self.total_bytes * 8,
            (self.file_header_bytes + self.group_header_bytes) * 8
                + self.occupied_inline_bits
                + self.overflow_waste_bits
                + self.final_waste_bits
                + self.padding_bits
        );
    }
}

fn aligned(value: u64, alignment: u32) -> u64 {
    value.div_ceil(u64::from(alignment)) * u64::from(alignment)
}

fn inline_limits(configuration: &Configuration) -> Record {
    std::array::from_fn(|column| (1u64 << configuration.widths[column]) - 1)
}

fn overflow(record: &Record, minimum: &Record, maximum: &Record, limits: &Record) -> u16 {
    let mut failures = 0;
    for (column, &absolute) in record.iter().enumerate() {
        let value = if column >= GRAMMAR_COLUMNS {
            maximum[column].max(absolute) - minimum[column].min(absolute)
        } else {
            absolute
        };
        if value > limits[column] {
            failures |= 1 << column;
        }
    }
    failures
}

fn simulate(nodes: &[Record], configuration: &Configuration) -> Option<Metrics> {
    let limits = inline_limits(configuration);
    let record_bits: u64 = configuration
        .widths
        .iter()
        .map(|&width| u64::from(width))
        .sum();
    let mut metrics = Metrics {
        nodes: nodes.len() as u64,
        ..Metrics::default()
    };
    let mut occupied = 0;
    let mut minimum = [0; 11];
    let mut maximum = [0; 11];
    for record in nodes {
        let transformed = squat::transform(record);
        let record = &transformed;
        if occupied == 0 {
            minimum = *record;
            maximum = *record;
        }
        let mut failures = overflow(record, &minimum, &maximum, &limits);
        if failures != 0 && occupied > 0 {
            metrics.overflow_waste_bits +=
                u64::from(configuration.capacity - occupied) * record_bits;
            *metrics.occupancy.entry(occupied).or_default() += 1;
            let trigger = COLUMNS
                .iter()
                .enumerate()
                .filter_map(|(column, &name)| (failures & (1 << column) != 0).then_some(name))
                .collect::<Vec<_>>()
                .join(",");
            *metrics.overflow_triggers.entry(trigger).or_default() += 1;
            occupied = 0;
            minimum = *record;
            maximum = *record;
            failures = overflow(record, &minimum, &maximum, &limits);
        }
        if failures != 0 {
            return None;
        }
        if occupied == 0 {
            metrics.groups += 1;
        }
        for column in GRAMMAR_COLUMNS..COLUMNS.len() {
            minimum[column] = minimum[column].min(record[column]);
            maximum[column] = maximum[column].max(record[column]);
        }
        occupied += 1;
        if occupied == configuration.capacity {
            *metrics.occupancy.entry(occupied).or_default() += 1;
            occupied = 0;
        }
    }
    if occupied > 0 {
        metrics.final_waste_bits = u64::from(configuration.capacity - occupied) * record_bits;
        *metrics.occupancy.entry(occupied).or_default() += 1;
    }
    metrics.occupied_inline_bits = nodes.len() as u64 * record_bits;
    squat::apply_layout(&mut metrics, configuration);
    if metrics.total_bytes > u64::from(u32::MAX) || nodes.len() > u32::MAX as usize {
        return None;
    }
    metrics.verify();
    Some(metrics)
}

fn validate(file: &Extracted) -> Result<()> {
    ensure!(
        file.schema == SCHEMA && file.runtime_revision == RUNTIME_REVISION,
        "unsupported extraction provenance"
    );
    ensure!(!file.nodes.is_empty(), "tree must include a root record");
    ensure!(
        file.nodes[0][4] == file.nodes.len() as u64,
        "root subtree count differs from node count"
    );
    let mut previous_start = 0;
    let mut previous_row = 0;
    let mut ancestors: Vec<(usize, usize)> = Vec::new();
    for (index, record) in file.nodes.iter().enumerate() {
        ensure!(
            record.iter().all(|&value| value <= u32::MAX as u64),
            "record exceeds schema's u32 domain"
        );
        ensure!(
            record[..4]
                .iter()
                .zip(file.bounds)
                .all(|(&value, bound)| value <= bound),
            "record exceeds declared grammar bound"
        );
        ensure!(
            record[5] >= previous_start && record[7] >= previous_row,
            "nonmonotone starts"
        );
        previous_start = record[5];
        previous_row = record[7];
        let end = index
            .checked_add(record[4] as usize)
            .context("subtree count overflow")?;
        ensure!(
            record[4] > 0 && end <= file.nodes.len(),
            "invalid subtree count"
        );
        ensure!(
            record[5] + record[6] <= file.source_bytes,
            "range exceeds source"
        );
        ensure!(
            record[7] + record[8] <= u64::from(u32::MAX),
            "end row exceeds u32"
        );
        if record[8] == 0 {
            ensure!(
                record[10] == record[6],
                "single-row column length differs from byte length"
            );
            ensure!(
                record[9] + record[10] <= u64::from(u32::MAX),
                "end column exceeds u32"
            );
        } else {
            ensure!(
                record[10] <= record[6],
                "final-row column length exceeds byte length"
            );
        }
        while ancestors.last().is_some_and(|&(_, end)| end == index) {
            ancestors.pop();
        }
        if let Some(&(parent, parent_end)) = ancestors.last() {
            ensure!(end <= parent_end, "crossing subtree intervals");
            ensure!(
                record[5] + record[6] <= file.nodes[parent][5] + file.nodes[parent][6],
                "child range exceeds parent"
            );
        }
        ancestors.push((index, end));
    }
    Ok(())
}

#[cfg(test)]
fn dominates(left: &Metrics, right: &Metrics) -> bool {
    let left = [
        left.total_bytes,
        left.overflow_waste_bits,
        left.group_header_bytes,
    ];
    let right = [
        right.total_bytes,
        right.overflow_waste_bits,
        right.group_header_bytes,
    ];
    left.iter().zip(right).all(|(&left, right)| left <= right)
        && left.iter().zip(right).any(|(&left, right)| left < right)
}

#[derive(Serialize)]
struct Evaluation {
    configuration_id: String,
    configuration: Configuration,
    totals: Metrics,
    invalid_files: Vec<String>,
    pareto: bool,
}

fn mark_frontier(evaluations: &mut [Evaluation]) {
    let key = |evaluation: &Evaluation| {
        (
            evaluation.totals.total_bytes,
            evaluation.totals.overflow_waste_bits,
            evaluation.totals.group_header_bytes,
        )
    };
    let mut order: Vec<usize> = (0..evaluations.len())
        .filter(|&index| evaluations[index].invalid_files.is_empty())
        .collect();
    order.sort_unstable_by_key(|&index| key(&evaluations[index]));
    let mut wastes: Vec<u64> = order
        .iter()
        .map(|&index| evaluations[index].totals.overflow_waste_bits)
        .collect();
    wastes.sort_unstable();
    wastes.dedup();
    // Sweep total bytes, maintaining prefix minima of header bytes by waste.
    // Equal triples are queried together so exact ties never dominate each other.
    let mut minimum_headers = vec![u64::MAX; wastes.len() + 1];
    let mut start = 0;
    while start < order.len() {
        let triple = key(&evaluations[order[start]]);
        let mut end = start + 1;
        while end < order.len() && key(&evaluations[order[end]]) == triple {
            end += 1;
        }
        let coordinate = wastes.binary_search(&triple.1).unwrap() + 1;
        let mut position = coordinate;
        let mut previous_minimum = u64::MAX;
        while position > 0 {
            previous_minimum = previous_minimum.min(minimum_headers[position]);
            position &= position - 1;
        }
        for &index in &order[start..end] {
            evaluations[index].pareto = previous_minimum > triple.2;
        }
        position = coordinate;
        while position < minimum_headers.len() {
            minimum_headers[position] = minimum_headers[position].min(triple.2);
            position += position & position.wrapping_neg();
        }
        start = end;
    }
}

fn writer(path: &str) -> Result<BufWriter<File>> {
    // Results are immutable run artifacts; avoid truncating an existing file.
    Ok(BufWriter::new(
        File::options().write(true).create_new(true).open(path)?,
    ))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    Ok(serde_json::from_reader(BufReader::new(File::open(path)?))?)
}

fn extract_files(arguments: &[String]) -> Result<()> {
    ensure!(
        arguments.len() >= 5,
        "extract LIBRARY SYMBOL GRAMMAR OUTPUT FILE..."
    );
    let library_hash = file_digest(&arguments[0])?;
    // The library remains alive until the language and every parser/tree drop.
    // Callers supply trusted native tree-sitter grammar libraries.
    let library = unsafe { libloading::Library::new(&arguments[0]) }?;
    let function =
        unsafe { library.get::<unsafe extern "C" fn() -> *const ()>(arguments[1].as_bytes()) }?;
    let language = Language::new(unsafe { LanguageFn::from_raw(*function) });
    let mut output = writer(&arguments[3])?;
    for path in &arguments[4..] {
        let source = fs::read(path).with_context(|| format!("reading {path}"))?;
        let (bounds, nodes) =
            extract(&source, &language).with_context(|| format!("parsing {path}"))?;
        let file = Extracted {
            schema: SCHEMA,
            path: path.clone(),
            grammar: arguments[2].clone(),
            grammar_sha256: library_hash.clone(),
            runtime_revision: RUNTIME_REVISION.into(),
            source_sha256: digest(&source),
            source_bytes: source.len() as u64,
            error_nodes: nodes.iter().filter(|record| record[3] & 8 != 0).count() as u64,
            bounds,
            nodes,
        };
        validate(&file)?;
        serde_json::to_writer(&mut output, &file)?;
        writeln!(output)?;
    }
    output.flush()?;
    Ok(())
}

fn analyze(arguments: &[String]) -> Result<()> {
    ensure!(arguments.len() == 3, "analyze RECORDS SEARCH OUTPUT");
    let _ = code_corpora_sha()?;
    let search: Search = read_json(&arguments[1])?;
    let mut evaluations: Vec<Evaluation> = Vec::new();
    let mut identity = None;
    let mut files = 0;
    let mut source_bytes = 0;
    let mut error_nodes = 0;
    let mut seen_paths = BTreeSet::new();
    let per_file_path = format!("{}.files.jsonl", arguments[2]);
    let mut per_file = writer(&per_file_path)?;
    for line in BufReader::new(File::open(&arguments[0])?).lines() {
        let file: Extracted = serde_json::from_str(&line?)?;
        validate(&file).with_context(|| file.path.clone())?;
        ensure!(
            seen_paths.insert(file.path.clone()),
            "duplicate path {}",
            file.path
        );
        let file_identity = (
            file.grammar.clone(),
            file.grammar_sha256.clone(),
            file.bounds,
        );
        if let Some(identity) = &identity {
            ensure!(
                identity == &file_identity,
                "analyze one grammar/build at a time"
            );
        } else {
            evaluations = configurations(&search, &file.bounds)?
                .into_iter()
                .map(|configuration| Evaluation {
                    configuration_id: configuration_id(&configuration),
                    configuration,
                    totals: Metrics::default(),
                    invalid_files: Vec::new(),
                    pareto: false,
                })
                .collect();
            identity = Some(file_identity);
        }
        for (index, evaluation) in evaluations.iter_mut().enumerate() {
            let result = simulate(&file.nodes, &evaluation.configuration);
            if let Some(metrics) = &result {
                evaluation.totals.add(metrics);
            } else {
                evaluation.invalid_files.push(file.path.clone());
            }
            serde_json::to_writer(
                &mut per_file,
                &serde_json::json!({
                    "path": file.path, "source_sha256": file.source_sha256,
                    "source_bytes": file.source_bytes, "configuration": index, "metrics": result,
                }),
            )?;
            writeln!(per_file)?;
        }
        files += 1;
        source_bytes += file.source_bytes;
        error_nodes += file.error_nodes;
    }
    ensure!(files > 0, "empty extraction input");
    mark_frontier(&mut evaluations);
    per_file.flush()?;
    let mut output = writer(&arguments[2])?;
    serde_json::to_writer_pretty(
        &mut output,
        &serde_json::json!({
            "schema": SCHEMA, "model": MODEL, "columns": COLUMNS, "grammar": identity,
            "code_corpora_sha": code_corpora_sha()?,
            "files": files, "source_bytes": source_bytes, "error_nodes": error_nodes,
            "search_sha256": digest(&fs::read(&arguments[1])?),
            "records_sha256": file_digest(&arguments[0])?,
            "search_coverage": "exhaustive for supplied domain and extraction input only",
            "objectives": ["total_bytes", "overflow_waste_bits", "group_header_bytes"],
            "configurations": evaluations, "per_file": per_file_path,
        }),
    )?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
}

fn main() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    run(&arguments)
}

pub fn run(arguments: &[String]) -> Result<()> {
    match arguments.first().map(String::as_str) {
        Some("extract") => extract_files(&arguments[1..]),
        Some("analyze") => analyze(&arguments[1..]),
        Some("run") => {
            if corpus::run(&arguments[1..])? {
                std::process::exit(130);
            }
            Ok(())
        }
        _ => bail!("usage: tree-sitter-memory-pareto extract|analyze|run ..."),
    }
}

mod corpus;
mod squat;
#[cfg(test)]
mod tests;
