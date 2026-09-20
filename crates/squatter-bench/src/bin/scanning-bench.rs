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
use tree_sitter::Point;
use tree_squatter::{Grammar, IdSet, KindSet, Node, Tree, traits::NodeLike};

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
    /// Reverse workload order before rotating it across samples.
    #[arg(long)]
    reverse_workloads: bool,
    /// Number of frequent named kinds selected by multi_kind workloads.
    #[arg(long, default_value_t = 4)]
    kind_count: usize,
    /// Query start as a percentage of source length.
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u8).range(0..=100))]
    range_start_percent: u8,
    /// Query width as a percentage of source length, clipped at EOF.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=100))]
    range_percent: u8,
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
    point_range: Range<Point>,
    nodes: usize,
    kind_matches: usize,
    multiple_kind_matches: usize,
    field: u16,
    field_matches: usize,
    range_matches: usize,
    supertype: u16,
    supertype_matches: usize,
    flags_matches: usize,
    combined_matches: usize,
    frequent_kind_ids: [u16; 16],
    sized_kind_sets: [KindSet; 5],
    sized_kind_matches: [usize; 5],
    frequent_field_ids: [u16; 4],
    sized_field_sets: [IdSet; 3],
    sized_field_matches: [usize; 3],
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
fn consume_fold<T>(nodes: impl Iterator<Item = T>) -> usize {
    nodes.fold(0, |count, node| {
        black_box(node);
        count + 1
    })
}
fn scalar_preorder(tree: &Tree) -> impl Iterator<Item = Node<'_>> {
    std::iter::successors(Some(tree.root_node()), |node| node.next_preorder())
}
fn overlaps(node: Node<'_>, range: &Range<usize>) -> bool {
    let start = node.start_byte();
    let end = node.end_byte();
    !range.is_empty() && start < range.end && (end > range.start || start >= range.start)
}
fn overlaps_points(node: Node<'_>, range: &Range<Point>) -> bool {
    let start = node.start_position();
    let end = node.end_position();
    !range.is_empty() && start < range.end && (end > range.start || start >= range.start)
}
fn source_point(source: &[u8], offset: usize) -> Point {
    let prefix = &source[..offset];
    Point::new(
        prefix.iter().filter(|&&byte| byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(offset, |newline| offset - newline - 1),
    )
}
type Operation = fn(&Case) -> usize;
fn fixed_kinds<const N: usize>(case: &Case) -> [u16; N] {
    case.frequent_kind_ids[..N].try_into().unwrap()
}
fn sized_kind_workloads<const N: usize>(
    names: [&'static str; 6],
) -> Vec<(&'static str, Operation)> {
    vec![
        (names[0], |case| {
            consume(
                case.tree
                    .root_node()
                    .all()
                    .filter_kind_ids(fixed_kinds::<N>(case))
                    .nodes(),
            )
        }),
        (names[1], |case| {
            case.tree
                .root_node()
                .all()
                .filter_kind_ids(fixed_kinds::<N>(case))
                .count()
        }),
        (names[2], |case| {
            consume_fold(
                case.tree
                    .root_node()
                    .all()
                    .filter_kind_ids(fixed_kinds::<N>(case))
                    .nodes(),
            )
        }),
        (names[3], |case| {
            consume(
                case.tree
                    .root_node()
                    .all()
                    .filter_kind_ids(&case.sized_kind_sets[N.ilog2() as usize])
                    .nodes(),
            )
        }),
        (names[4], |case| {
            case.tree
                .root_node()
                .all()
                .filter_kind_ids(&case.sized_kind_sets[N.ilog2() as usize])
                .count()
        }),
        (names[5], |case| {
            consume_fold(
                case.tree
                    .root_node()
                    .all()
                    .filter_kind_ids(&case.sized_kind_sets[N.ilog2() as usize])
                    .nodes(),
            )
        }),
    ]
}
fn workloads() -> Vec<(&'static str, Operation)> {
    let mut workloads: Vec<(&'static str, Operation)> = vec![
        ("preorder.fold", |case| {
            consume_fold(case.tree.root_node().preorder().nodes())
        }),
        ("preorder.rev.fold", |case| {
            case.tree
                .root_node()
                .preorder()
                .rev()
                .nodes()
                .fold(0, |count, node| {
                    black_box(node);
                    count + 1
                })
        }),
        ("preorder.groups.fold", |case| {
            case.tree
                .root_node()
                .preorder()
                .groups()
                .map(|group| consume_fold(group.nodes()))
                .sum()
        }),
        ("postorder.fold", |case| {
            consume_fold(case.tree.root_node().postorder().nodes())
        }),
        ("postorder.rev.fold", |case| {
            consume_fold(case.tree.root_node().postorder().rev().nodes())
        }),
        ("kind.fold", |case| {
            consume_fold(
                case.tree
                    .root_node()
                    .all()
                    .filter_kind_ids(&case.kinds)
                    .nodes(),
            )
        }),
        ("multi_kind.fold", |case| {
            consume_fold(
                case.tree
                    .root_node()
                    .all()
                    .filter_kind_ids(&case.multiple_kinds)
                    .nodes(),
            )
        }),
        ("supertype.nodes", |case| {
            consume(
                case.tree
                    .root_node()
                    .all()
                    .filter_supertype_id(case.supertype)
                    .nodes(),
            )
        }),
        ("supertype.count", |case| {
            case.tree
                .root_node()
                .all()
                .filter_supertype_id(case.supertype)
                .count()
        }),
        ("flags.count", |case| {
            case.tree
                .root_node()
                .all()
                .filter_extra(false)
                .filter_missing(false)
                .count()
        }),
        ("combined.count", |case| {
            case.tree
                .root_node()
                .all()
                .filter_kind_ids(&case.kinds)
                .filter_field_id(case.field)
                .filter_extra(false)
                .filter_missing(false)
                .count()
        }),
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
            consume(scalar_preorder(&case.tree).filter(|node| node.field_id() == case.field))
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
        ("scalar_next_preorder", |case| {
            consume(scalar_preorder(&case.tree))
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
            consume(scalar_preorder(&case.tree).filter(|node| case.kinds.contains(node.kind_id())))
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
        ("range.fold", |case| {
            consume_fold(
                case.tree
                    .root_node()
                    .all()
                    .overlapping_bytes(case.range.clone())
                    .nodes(),
            )
        }),
        ("range.scalar", |case| {
            consume(scalar_preorder(&case.tree).filter(|&node| overlaps(node, &case.range)))
        }),
        ("point_range.nodes", |case| {
            consume(
                case.tree
                    .root_node()
                    .all()
                    .overlapping_points(case.point_range.clone())
                    .nodes(),
            )
        }),
        ("point_range.count", |case| {
            case.tree
                .root_node()
                .all()
                .overlapping_points(case.point_range.clone())
                .count()
        }),
        ("point_range.fold", |case| {
            consume_fold(
                case.tree
                    .root_node()
                    .all()
                    .overlapping_points(case.point_range.clone())
                    .nodes(),
            )
        }),
        ("point_range.scalar", |case| {
            consume(
                scalar_preorder(&case.tree)
                    .filter(|&node| overlaps_points(node, &case.point_range)),
            )
        }),
    ];
    workloads.extend(sized_kind_workloads::<1>([
        "fixed_1.nodes",
        "fixed_1.count",
        "fixed_1.fold",
        "dynamic_1.nodes",
        "dynamic_1.count",
        "dynamic_1.fold",
    ]));
    workloads.extend(sized_kind_workloads::<2>([
        "fixed_2.nodes",
        "fixed_2.count",
        "fixed_2.fold",
        "dynamic_2.nodes",
        "dynamic_2.count",
        "dynamic_2.fold",
    ]));
    workloads.extend(sized_kind_workloads::<4>([
        "fixed_4.nodes",
        "fixed_4.count",
        "fixed_4.fold",
        "dynamic_4.nodes",
        "dynamic_4.count",
        "dynamic_4.fold",
    ]));
    workloads.extend(sized_kind_workloads::<8>([
        "fixed_8.nodes",
        "fixed_8.count",
        "fixed_8.fold",
        "dynamic_8.nodes",
        "dynamic_8.count",
        "dynamic_8.fold",
    ]));
    workloads.extend(sized_kind_workloads::<16>([
        "fixed_16.nodes",
        "fixed_16.count",
        "fixed_16.fold",
        "dynamic_16.nodes",
        "dynamic_16.count",
        "dynamic_16.fold",
    ]));
    workloads.extend(sized_field_workloads::<1>([
        "fixed_field_1.nodes",
        "fixed_field_1.count",
        "dynamic_field_1.nodes",
        "dynamic_field_1.count",
        "scalar_field_1.nodes",
    ]));
    workloads.extend(sized_field_workloads::<2>([
        "fixed_field_2.nodes",
        "fixed_field_2.count",
        "dynamic_field_2.nodes",
        "dynamic_field_2.count",
        "scalar_field_2.nodes",
    ]));
    workloads.extend(sized_field_workloads::<4>([
        "fixed_field_4.nodes",
        "fixed_field_4.count",
        "dynamic_field_4.nodes",
        "dynamic_field_4.count",
        "scalar_field_4.nodes",
    ]));
    workloads
}
fn fixed_fields<const N: usize>(case: &Case) -> [u16; N] {
    case.frequent_field_ids[..N].try_into().unwrap()
}
fn sized_field_workloads<const N: usize>(
    names: [&'static str; 5],
) -> Vec<(&'static str, Operation)> {
    vec![
        (names[0], |case| {
            consume(
                case.tree
                    .root_node()
                    .all()
                    .filter_field_ids(fixed_fields::<N>(case))
                    .nodes(),
            )
        }),
        (names[1], |case| {
            case.tree
                .root_node()
                .all()
                .filter_field_ids(fixed_fields::<N>(case))
                .count()
        }),
        (names[2], |case| {
            consume(
                case.tree
                    .root_node()
                    .all()
                    .filter_field_ids(&case.sized_field_sets[N.ilog2() as usize])
                    .nodes(),
            )
        }),
        (names[3], |case| {
            case.tree
                .root_node()
                .all()
                .filter_field_ids(&case.sized_field_sets[N.ilog2() as usize])
                .count()
        }),
        (names[4], |case| {
            consume(
                scalar_preorder(&case.tree)
                    .filter(|node| fixed_fields::<N>(case).contains(&node.field_id())),
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
    let preorder: Vec<_> = scalar_preorder(&case.tree).collect();
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
    let point_matches = preorder
        .iter()
        .copied()
        .filter(|&node| overlaps_points(node, &case.point_range))
        .collect::<Vec<_>>();
    assert_eq!(point_matches.len(), case.range_matches);
    assert_eq!(
        root.all()
            .overlapping_points(case.point_range.clone())
            .nodes()
            .collect::<Vec<_>>(),
        point_matches
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
    assert_eq!(
        root.all()
            .filter_supertype_id(case.supertype)
            .nodes()
            .collect::<Vec<_>>(),
        preorder
            .iter()
            .copied()
            .filter(|node| node.has_supertype(case.supertype))
            .collect::<Vec<_>>()
    );
}
#[cfg(target_os = "linux")]
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
#[cfg(not(target_os = "linux"))]
fn pin(cpu: usize) -> Result<()> {
    anyhow::bail!("CPU affinity {cpu} is only supported on Linux")
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
        for node in scalar_preorder(&tree) {
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
        let frequent_kind_ids =
            std::array::from_fn(|index| ranked_kinds.get(index).map_or(kind, |&(&kind, _)| kind));
        let sized_kind_sets = std::array::from_fn(|index| {
            KindSet::new(frequent_kind_ids[..1 << index].iter().copied())
        });
        let mut sized_kind_matches = [0; 5];
        for node in scalar_preorder(&tree) {
            for (count, kinds) in sized_kind_matches.iter_mut().zip(&sized_kind_sets) {
                *count += usize::from(kinds.contains(node.kind_id()));
            }
        }
        let multiple_kinds = KindSet::new(
            ranked_kinds
                .into_iter()
                .take(arguments.kind_count)
                .map(|(&kind, _)| kind),
        );
        let multiple_kind_matches = scalar_preorder(&tree)
            .filter(|node| multiple_kinds.contains(node.kind_id()))
            .count();
        let field = fields
            .iter()
            .max_by_key(|&(field, count)| (*count, std::cmp::Reverse(*field)))
            .map_or(0, |(&field, _)| field);
        let mut ranked_fields = fields.iter().collect::<Vec<_>>();
        ranked_fields.sort_by_key(|&(&field, &count)| (std::cmp::Reverse(count), field));
        let frequent_field_ids = std::array::from_fn(|index| {
            ranked_fields.get(index).map_or(field, |&(&field, _)| field)
        });
        let sized_field_sets = std::array::from_fn(|index| {
            IdSet::new(frequent_field_ids[..1 << index].iter().copied())
        });
        let mut sized_field_matches = [0; 3];
        for node in scalar_preorder(&tree) {
            for (count, fields) in sized_field_matches.iter_mut().zip(&sized_field_sets) {
                *count += usize::from(fields.contains(node.field_id()));
            }
        }
        let field_matches = scalar_preorder(&tree)
            .filter(|node| node.field_id() == field)
            .count();
        let range_start = source.len() * usize::from(arguments.range_start_percent) / 100;
        let range_length = (source.len() * usize::from(arguments.range_percent) / 100).max(1);
        let range = range_start..(range_start + range_length).min(source.len());
        let point_range = source_point(&source, range.start)..source_point(&source, range.end);
        let kind_matches = scalar_preorder(&tree)
            .filter(|node| kinds.contains(node.kind_id()))
            .count();
        let range_matches = scalar_preorder(&tree)
            .filter(|&node| overlaps(node, &range))
            .count();
        let supertype = language.supertypes().first().copied().unwrap_or(u16::MAX);
        let supertype_matches = scalar_preorder(&tree)
            .filter(|node| node.has_supertype(supertype))
            .count();
        let flags_matches = scalar_preorder(&tree)
            .filter(|node| !node.is_extra() && !node.is_missing())
            .count();
        let combined_matches = scalar_preorder(&tree)
            .filter(|node| {
                kinds.contains(node.kind_id())
                    && node.field_id() == field
                    && !node.is_extra()
                    && !node.is_missing()
            })
            .count();
        descriptions.push(serde_json::json!({
            "input": input, "source_bytes": source.len(), "slab_bytes": tree.as_bytes().len(),
            "nodes": nodes, "groups": tree.group_count(), "kind_id": kind,
            "kind": language.node_kind_for_id(kind), "kind_matches": kind_matches,
            "multiple_kind_matches": multiple_kind_matches,
            "field_id": field, "field_matches": field_matches,
            "range": [range.start, range.end], "range_matches": range_matches,
            "point_range": [
                [point_range.start.row, point_range.start.column],
                [point_range.end.row, point_range.end.column],
            ],
            "supertype_id": supertype, "supertype_count": language.supertypes().len(),
            "supertype_matches": supertype_matches,
            "flags_matches": flags_matches, "combined_matches": combined_matches,
            "frequent_kind_ids": frequent_kind_ids, "sized_kind_matches": sized_kind_matches,
            "frequent_field_ids": frequent_field_ids, "sized_field_matches": sized_field_matches,
        }));
        let case = Case {
            tree,
            native,
            kinds,
            multiple_kinds,
            range,
            point_range,
            nodes,
            kind_matches,
            multiple_kind_matches,
            field,
            field_matches,
            range_matches,
            supertype,
            supertype_matches,
            flags_matches,
            combined_matches,
            frequent_kind_ids,
            sized_kind_sets,
            sized_kind_matches,
            frequent_field_ids,
            sized_field_sets,
            sized_field_matches,
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
    if arguments.reverse_workloads {
        workloads.reverse();
    }
    let mut results = Vec::new();
    for &(name, operation) in &workloads {
        let expected: usize = cases
            .iter()
            .map(|case| {
                if let Some(sized) = name
                    .strip_prefix("fixed_field_")
                    .or_else(|| name.strip_prefix("dynamic_field_"))
                    .or_else(|| name.strip_prefix("scalar_field_"))
                {
                    let length = sized.split('.').next().unwrap().parse::<usize>().unwrap();
                    case.sized_field_matches[length.ilog2() as usize]
                } else if let Some(sized) = name
                    .strip_prefix("fixed_")
                    .or_else(|| name.strip_prefix("dynamic_"))
                {
                    let length = sized.split('.').next().unwrap().parse::<usize>().unwrap();
                    case.sized_kind_matches[length.ilog2() as usize]
                } else if name.starts_with("multi_kind.") {
                    case.multiple_kind_matches
                } else if name.starts_with("kind.") || name.contains(".kind.") {
                    case.kind_matches
                } else if name.starts_with("field.") || name.contains(".field.") {
                    case.field_matches
                } else if name.starts_with("range.") || name.starts_with("point_range.") {
                    case.range_matches
                } else if name.starts_with("supertype.") {
                    case.supertype_matches
                } else if name == "flags.count" {
                    case.flags_matches
                } else if name == "combined.count" {
                    case.combined_matches
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
