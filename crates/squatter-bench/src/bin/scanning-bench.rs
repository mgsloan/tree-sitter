//! Corpus throughput for the group-scan prototype; parsing and packing are setup.
use anyhow::{Result, ensure};
use clap::Parser;
use corpus_analysis::{LoadedGrammar, Registry, digest, parse};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    hint::black_box,
    ops::Range,
    path::PathBuf,
    time::{Duration, Instant},
};
use tree_squatter::{Grammar, KindSet, Node, Tree, traits::NodeLike};

#[derive(Parser, Serialize)]
struct Arguments {
    #[arg(long)]
    registry: PathBuf,
    /// JSON array containing path, grammar, and sha256 for each input.
    #[arg(long)]
    inputs: PathBuf,
    #[arg(long)]
    corpus: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 7)]
    samples: usize,
    #[arg(long, default_value_t = 40)]
    sample_ms: u64,
    #[arg(long)]
    cpu: Option<usize>,
    /// Restrict timing to these workload names.
    #[arg(long)]
    workload: Vec<String>,
}
#[derive(Deserialize, Serialize)]
struct Input {
    path: String,
    grammar: String,
    sha256: String,
}
struct Case {
    tree: Tree,
    native: tree_sitter::Tree,
    kinds: KindSet,
    multiple_kinds: KindSet,
    range: Range<usize>,
    nodes: usize,
    kind_matches: usize,
    multiple_kind_matches: usize,
    field: u16,
    field_matches: usize,
    range_matches: usize,
}
#[derive(Serialize)]
struct ResultRow {
    workload: &'static str,
    iterations_per_sample: usize,
    input_nodes_per_iteration: usize,
    output_nodes_per_iteration: usize,
    seconds: Vec<f64>,
    median_input_nodes_per_second: f64,
    median_output_nodes_per_second: f64,
    min_input_nodes_per_second: f64,
    max_input_nodes_per_second: f64,
}

fn consume<T>(nodes: impl Iterator<Item = T>) -> usize {
    let mut count = 0;
    for node in nodes {
        black_box(node);
        count += 1;
    }
    count
}
fn overlaps(node: Node<'_>, range: &Range<usize>) -> bool {
    let start = node.start_byte();
    let end = node.end_byte();
    start < end && start < range.end && end > range.start
}
type Operation = fn(&Case) -> usize;
fn workloads() -> Vec<(&'static str, Operation)> {
    vec![
        ("multi_kind.nodes", |case| {
            consume(
                case.tree
                    .root_node()
                    .all()
                    .filter_kind_ids(&case.multiple_kinds)
                    .nodes(),
            )
        }),
        ("multi_kind.count", |case| {
            case.tree
                .root_node()
                .all()
                .filter_kind_ids(&case.multiple_kinds)
                .count()
        }),
        ("field.nodes", |case| {
            consume(
                case.tree
                    .root_node()
                    .all()
                    .filter_field_id(case.field)
                    .nodes(),
            )
        }),
        ("field.count", |case| {
            case.tree
                .root_node()
                .all()
                .filter_field_id(case.field)
                .count()
        }),
        ("field.scalar", |case| {
            consume(
                case.tree
                    .root_node()
                    .node_iterator()
                    .unwrap()
                    .filter(|node| node.field_id() == case.field),
            )
        }),
        ("postorder.field.nodes", |case| {
            consume(
                case.tree
                    .root_node()
                    .postorder()
                    .filter_field_id(case.field)
                    .nodes(),
            )
        }),
        ("postorder.field.count", |case| {
            case.tree
                .root_node()
                .postorder()
                .filter_field_id(case.field)
                .count()
        }),
        ("postorder.kind.nodes", |case| {
            consume(
                case.tree
                    .root_node()
                    .postorder()
                    .filter_kind_ids(&case.kinds)
                    .nodes(),
            )
        }),
        ("postorder.kind.count", |case| {
            case.tree
                .root_node()
                .postorder()
                .filter_kind_ids(&case.kinds)
                .count()
        }),
        ("postorder.rev.kind.nodes", |case| {
            consume(
                case.tree
                    .root_node()
                    .postorder()
                    .rev()
                    .filter_kind_ids(&case.kinds)
                    .nodes(),
            )
        }),
        ("preorder.nodes.rev", |case| {
            consume(case.tree.root_node().preorder().nodes().rev())
        }),
        ("preorder.nodes", |case| {
            consume(case.tree.root_node().preorder().nodes())
        }),
        ("preorder.rev.nodes", |case| {
            consume(case.tree.root_node().preorder().rev().nodes())
        }),
        ("postorder.nodes", |case| {
            consume(case.tree.root_node().postorder().nodes())
        }),
        ("postorder.rev.nodes", |case| {
            consume(case.tree.root_node().postorder().rev().nodes())
        }),
        ("all.nodes", |case| {
            consume(case.tree.root_node().all().nodes())
        }),
        ("all.rev.nodes", |case| {
            consume(case.tree.root_node().all().rev().nodes())
        }),
        ("native_iterator", |case| {
            consume(case.tree.root_node().node_iterator().unwrap())
        }),
        ("scalar_next_preorder", |case| {
            consume(std::iter::successors(Some(case.tree.root_node()), |node| {
                node.next_preorder()
            }))
        }),
        ("mainline_cursor", |case| {
            consume(case.native.root_node().preorder())
        }),
        ("preorder.count", |case| {
            case.tree.root_node().preorder().count()
        }),
        ("preorder.nodes.count", |case| {
            case.tree.root_node().preorder().nodes().count()
        }),
        ("postorder.count", |case| {
            case.tree.root_node().postorder().count()
        }),
        ("all.count", |case| case.tree.root_node().all().count()),
        ("kind.nodes", |case| {
            consume(
                case.tree
                    .root_node()
                    .all()
                    .filter_kind_ids(&case.kinds)
                    .nodes(),
            )
        }),
        ("kind.count", |case| {
            case.tree
                .root_node()
                .all()
                .filter_kind_ids(&case.kinds)
                .count()
        }),
        ("kind.scalar", |case| {
            consume(
                case.tree
                    .root_node()
                    .node_iterator()
                    .unwrap()
                    .filter(|node| case.kinds.contains(node.kind_id())),
            )
        }),
        ("range.nodes", |case| {
            consume(
                case.tree
                    .root_node()
                    .all()
                    .overlapping_bytes(case.range.clone())
                    .nodes(),
            )
        }),
        ("range.count", |case| {
            case.tree
                .root_node()
                .all()
                .overlapping_bytes(case.range.clone())
                .count()
        }),
        ("range.scalar", |case| {
            consume(
                case.tree
                    .root_node()
                    .node_iterator()
                    .unwrap()
                    .filter(|&node| overlaps(node, &case.range)),
            )
        }),
    ]
}
fn run(operation: Operation, cases: &[Case], iterations: usize) -> usize {
    let mut count = 0;
    for _ in 0..iterations {
        for case in cases {
            count += black_box(operation(black_box(case)));
        }
    }
    count
}
fn validate(case: &Case) {
    let root = case.tree.root_node();
    let preorder: Vec<_> = root.node_iterator().unwrap().collect();
    assert_eq!(root.preorder().nodes().collect::<Vec<_>>(), preorder);
    assert_eq!(root.all().nodes().collect::<Vec<_>>(), preorder);
    assert_eq!(
        root.preorder().rev().nodes().collect::<Vec<_>>(),
        preorder.iter().rev().copied().collect::<Vec<_>>()
    );
    let mut cursor = root.walk().unwrap();
    let mut postorder = Vec::new();
    'walk: loop {
        while cursor.goto_first_child() {}
        postorder.push(cursor.node());
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
            postorder.push(cursor.node());
        }
    }
    assert_eq!(root.postorder().nodes().collect::<Vec<_>>(), postorder);
    assert_eq!(
        root.postorder().rev().nodes().collect::<Vec<_>>(),
        postorder.iter().rev().copied().collect::<Vec<_>>()
    );
    assert_eq!(
        root.all()
            .filter_kind_ids(&case.kinds)
            .nodes()
            .collect::<Vec<_>>(),
        preorder
            .iter()
            .copied()
            .filter(|node| case.kinds.contains(node.kind_id()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        root.all()
            .overlapping_bytes(case.range.clone())
            .nodes()
            .collect::<Vec<_>>(),
        preorder
            .iter()
            .copied()
            .filter(|&node| overlaps(node, &case.range))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        root.all()
            .filter_field_id(case.field)
            .nodes()
            .collect::<Vec<_>>(),
        preorder
            .iter()
            .copied()
            .filter(|node| node.field_id() == case.field)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        root.all()
            .filter_kind_ids(&case.multiple_kinds)
            .nodes()
            .collect::<Vec<_>>(),
        preorder
            .iter()
            .copied()
            .filter(|node| case.multiple_kinds.contains(node.kind_id()))
            .collect::<Vec<_>>()
    );
}
fn pin(cpu: usize) -> Result<()> {
    ensure!(cpu < libc::CPU_SETSIZE as usize, "CPU out of range");
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        ensure!(
            libc::sched_setaffinity(0, std::mem::size_of_val(&set), &set) == 0,
            "set affinity: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}
fn main() -> Result<()> {
    let arguments = Arguments::parse();
    ensure!(
        arguments.samples > 0 && arguments.sample_ms > 0,
        "positive samples and duration required"
    );
    if let Some(cpu) = arguments.cpu {
        pin(cpu)?;
    }
    let registry = Registry::read(&arguments.registry)?;
    let inputs: Vec<Input> = serde_json::from_slice(&fs::read(&arguments.inputs)?)?;
    ensure!(!inputs.is_empty(), "no inputs");
    // Keep DSOs alive until all parsers, prepared grammars, and trees are dropped.
    let mut grammars = BTreeMap::new();
    for input in &inputs {
        if !grammars.contains_key(&input.grammar) {
            let grammar = unsafe { LoadedGrammar::open(&registry.grammars[&input.grammar]) }?;
            grammars.insert(input.grammar.clone(), grammar);
        }
    }
    let mut cases = Vec::new();
    let mut descriptions = Vec::new();
    for input in inputs {
        let source = fs::read(arguments.corpus.join(&input.path))?;
        ensure!(
            digest(&source) == input.sha256,
            "input hash changed: {}",
            input.path
        );
        let language = &grammars[&input.grammar].language;
        let grammar = Grammar::new(language)?;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(language)?;
        let native = parse(&mut parser, &source, Duration::from_secs(10))?;
        let tree = Tree::pack(&grammar, &native)?;
        let mut frequencies = BTreeMap::new();
        let mut fields = BTreeMap::new();
        let mut nodes = 0;
        for node in tree.root_node().node_iterator()? {
            nodes += 1;
            if node.field_id() != 0 {
                *fields.entry(node.field_id()).or_insert(0usize) += 1;
            }
            if node.is_named() {
                *frequencies.entry(node.kind_id()).or_insert(0usize) += 1;
            }
        }
        let kind = *frequencies
            .iter()
            .max_by_key(|&(kind, count)| (*count, std::cmp::Reverse(*kind)))
            .unwrap()
            .0;
        let kinds = KindSet::new([kind]);
        let mut ranked_kinds = frequencies.iter().collect::<Vec<_>>();
        ranked_kinds.sort_by_key(|&(&kind, &count)| (std::cmp::Reverse(count), kind));
        let multiple_kinds = KindSet::new(ranked_kinds.into_iter().take(4).map(|(&kind, _)| kind));
        let multiple_kind_matches = tree
            .root_node()
            .node_iterator()?
            .filter(|node| multiple_kinds.contains(node.kind_id()))
            .count();
        let field = fields
            .iter()
            .max_by_key(|&(field, count)| (*count, std::cmp::Reverse(*field)))
            .map_or(0, |(&field, _)| field);
        let field_matches = tree
            .root_node()
            .node_iterator()?
            .filter(|node| node.field_id() == field)
            .count();
        let range =
            source.len() / 2..(source.len() / 2 + (source.len() / 100).max(1)).min(source.len());
        let kind_matches = tree
            .root_node()
            .node_iterator()?
            .filter(|node| kinds.contains(node.kind_id()))
            .count();
        let range_matches = tree
            .root_node()
            .node_iterator()?
            .filter(|&node| overlaps(node, &range))
            .count();
        descriptions.push(serde_json::json!({
            "input": input, "source_bytes": source.len(), "slab_bytes": tree.as_bytes().len(),
            "nodes": nodes, "groups": tree.group_count(), "kind_id": kind,
            "kind": language.node_kind_for_id(kind), "kind_matches": kind_matches,
            "multiple_kind_matches": multiple_kind_matches,
            "field_id": field, "field_matches": field_matches,
            "range": [range.start, range.end], "range_matches": range_matches,
        }));
        let case = Case {
            tree,
            native,
            kinds,
            multiple_kinds,
            range,
            nodes,
            kind_matches,
            multiple_kind_matches,
            field,
            field_matches,
            range_matches,
        };
        validate(&case);
        cases.push(case);
    }
    let input_nodes = cases.iter().map(|case| case.nodes).sum::<usize>();
    eprintln!(
        "validated {} files, {} languages, {} nodes",
        cases.len(),
        grammars.len(),
        input_nodes
    );
    let mut workloads = workloads();
    for name in &arguments.workload {
        ensure!(
            workloads.iter().any(|(candidate, _)| candidate == name),
            "unknown workload: {name}"
        );
    }
    if !arguments.workload.is_empty() {
        workloads.retain(|(name, _)| arguments.workload.iter().any(|selected| selected == name));
    }
    let mut results = Vec::new();
    for &(name, operation) in &workloads {
        let expected: usize = cases
            .iter()
            .map(|case| {
                if name.starts_with("multi_kind.") {
                    case.multiple_kind_matches
                } else if name.starts_with("kind.") || name.contains(".kind.") {
                    case.kind_matches
                } else if name.starts_with("field.") || name.contains(".field.") {
                    case.field_matches
                } else if name.starts_with("range.") {
                    case.range_matches
                } else {
                    case.nodes
                }
            })
            .sum();
        assert_eq!(run(operation, &cases, 1), expected, "{name}");
        let start = Instant::now();
        let count = run(operation, &cases, 1);
        let seconds = start.elapsed().as_secs_f64();
        assert_eq!(count, expected, "{name}");
        let iterations =
            ((arguments.sample_ms as f64 / 1000.0 / seconds).ceil() as usize).clamp(1, 10000);
        results.push(ResultRow {
            workload: name,
            iterations_per_sample: iterations,
            input_nodes_per_iteration: input_nodes,
            output_nodes_per_iteration: expected,
            seconds: Vec::new(),
            median_input_nodes_per_second: 0.0,
            median_output_nodes_per_second: 0.0,
            min_input_nodes_per_second: 0.0,
            max_input_nodes_per_second: 0.0,
        });
    }
    for round in 0..arguments.samples {
        for offset in 0..workloads.len() {
            let index = (offset + round * 7) % workloads.len();
            let (_, operation) = workloads[index];
            let result = &mut results[index];
            let start = Instant::now();
            let count = run(operation, &cases, result.iterations_per_sample);
            let seconds = start.elapsed().as_secs_f64();
            assert_eq!(
                count,
                result.output_nodes_per_iteration * result.iterations_per_sample,
                "{}",
                result.workload
            );
            result.seconds.push(seconds);
        }
        eprintln!("sample {}/{} complete", round + 1, arguments.samples);
    }
    println!("workload,input_nodes/s,output_nodes/s,min_input_nodes/s,max_input_nodes/s");
    for result in &mut results {
        let mut times = result.seconds.clone();
        times.sort_by(f64::total_cmp);
        let median = (times[(times.len() - 1) / 2] + times[times.len() / 2]) / 2.0;
        let input_count = (input_nodes * result.iterations_per_sample) as f64;
        result.median_input_nodes_per_second = input_count / median;
        result.median_output_nodes_per_second =
            (result.output_nodes_per_iteration * result.iterations_per_sample) as f64 / median;
        result.min_input_nodes_per_second = input_count / times[times.len() - 1];
        result.max_input_nodes_per_second = input_count / times[0];
        println!(
            "{},{:.0},{:.0},{:.0},{:.0}",
            result.workload,
            result.median_input_nodes_per_second,
            result.median_output_nodes_per_second,
            result.min_input_nodes_per_second,
            result.max_input_nodes_per_second
        );
    }
    let report = serde_json::json!({
        "arguments": arguments, "inputs": descriptions, "results": results,
        "grammar_sha256": grammars.iter().map(|(name, grammar)| (name, &grammar.sha256)).collect::<BTreeMap<_, _>>(),
        "cpuinfo": fs::read_to_string("/proc/cpuinfo").ok(),
        "contract": "release build; cyclic corpus; scan construction included; parsing, packing and validation excluded; black_box each enumerated node; count consumes only aggregate; input nodes/s includes nodes skipped by group/range operations; median wall-clock throughput; workload order rotates each sample",
    });
    fs::write(&arguments.output, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}
