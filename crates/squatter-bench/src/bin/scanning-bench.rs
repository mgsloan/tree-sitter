//! Corpus throughput for the group-scan prototype; parsing and packing are setup.
use anyhow::{Result, ensure};
use clap::{Parser, ValueEnum};
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
use tree_squatter::{Forest, KindSet, Language, Node, PackOptions, traits::NodeLike};

#[derive(Clone, Copy, Serialize, ValueEnum)]
enum KindSelection {
    Frequent,
    Rare,
    Sparse,
    Absent,
}

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
    /// Select frequent, rare, sparse, or absent kind IDs.
    #[arg(long, value_enum, default_value = "frequent")]
    kind_selection: KindSelection,
    #[arg(long)]
    no_symbol_index: bool,
    /// Query start as a percentage of source length.
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u8).range(0..=100))]
    range_start_percent: u8,
    /// Query width as a percentage of source length, clipped at EOF.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=100))]
    range_percent: u8,
}
use tree_squatter::{FieldSet, GrammarId, KindId};
type FieldSelection = Option<tree_squatter::FieldId>;
fn raw_field(field: FieldSelection) -> u16 {
    field.map_or(0, tree_squatter::FieldId::raw)
}

#[derive(Deserialize, Serialize)]
struct Input {
    path: String,
    grammar: String,
    sha256: String,
}
struct Case {
    tree: Forest,
    native: tree_sitter::Tree,
    kinds: KindSet,
    multiple_kinds: KindSet,
    range: Range<usize>,
    point_range: Range<Point>,
    nodes: usize,
    kind_matches: usize,
    multiple_kind_matches: usize,
    field: FieldSelection,
    field_matches: usize,
    range_matches: usize,
    range_kind_matches: [usize; 5],
    within_matches: usize,
    within_kind_matches: [usize; 5],
    field_kind_matches: [usize; 5],
    flags_kind_matches: [usize; 5],
    range_field_kind_matches: [usize; 5],
    intersecting_kinds: KindSet,
    intersection_matches: [usize; 5],
    starting_in_matches: usize,
    starting_at_matches: usize,
    supertype: GrammarId,
    supertype_matches: usize,
    flags_matches: usize,
    combined_matches: usize,
    selected_kind_ids: [KindId; 16],
    sized_kind_sets: [KindSet; 5],
    sized_kind_matches: [usize; 5],
    frequent_field_ids: [FieldSelection; 4],
    sized_field_sets: [FieldSet; 3],
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
fn scalar_preorder(tree: &Forest) -> impl Iterator<Item = Node<'_>> {
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
struct Workload {
    name: &'static str,
    operation: Operation,
    expected: Operation,
}

fn fixed_kinds<const N: usize>(case: &Case) -> [KindId; N] {
    case.selected_kind_ids[..N].try_into().unwrap()
}

fn fixed_fields<const N: usize>(case: &Case) -> [FieldSelection; N] {
    case.frequent_field_ids[..N].try_into().unwrap()
}

fn workloads(cases: &[Case]) -> Vec<Workload> {
    let mut workloads = Vec::new();
    macro_rules! workload {
        ($name:expr, $case:ident, $operation:expr, $expected:expr) => {
            workloads.push(Workload {
                name: $name,
                operation: |$case| $operation,
                expected: |$case| $expected,
            });
        };
    }
    macro_rules! scan_workloads {
        ($name:expr, $case:ident, $scan:expr, $expected:expr, [$($consumer:ident),+]) => {$(
            scan_workloads!(@consumer $consumer, $name, $case, $scan, $expected);
        )+};
        (@consumer nodes, $name:expr, $case:ident, $scan:expr, $expected:expr) => {
            workload!(concat!($name, ".nodes"), $case, consume($scan.nodes()), $expected);
        };
        (@consumer count, $name:expr, $case:ident, $scan:expr, $expected:expr) => {
            workload!(concat!($name, ".count"), $case, $scan.count(), $expected);
        };
        (@consumer fold, $name:expr, $case:ident, $scan:expr, $expected:expr) => {
            workload!(concat!($name, ".fold"), $case, consume_fold($scan.nodes()), $expected);
        };
        (@consumer reverse_nodes, $name:expr, $case:ident, $scan:expr, $expected:expr) => {
            workload!(concat!($name, ".reverse_nodes"), $case, consume($scan.rev().nodes()), $expected);
        };
    }
    scan_workloads!(
        "preorder",
        case,
        case.tree.root_node().preorder(),
        case.nodes,
        [nodes, count, fold]
    );
    scan_workloads!(
        "preorder.rev",
        case,
        case.tree.root_node().preorder().rev(),
        case.nodes,
        [nodes, fold]
    );
    scan_workloads!(
        "postorder",
        case,
        case.tree.root_node().postorder(),
        case.nodes,
        [nodes, count, fold]
    );
    scan_workloads!(
        "postorder.rev",
        case,
        case.tree.root_node().postorder().rev(),
        case.nodes,
        [nodes, fold]
    );
    scan_workloads!(
        "all",
        case,
        case.tree.root_node().all(),
        case.nodes,
        [nodes, count]
    );
    scan_workloads!(
        "all.rev",
        case,
        case.tree.root_node().all().rev(),
        case.nodes,
        [nodes]
    );
    workload!(
        "preorder.groups.fold",
        case,
        case.tree
            .root_node()
            .preorder()
            .groups()
            .map(|group| consume_fold(group.nodes()))
            .sum(),
        case.nodes
    );
    workload!(
        "preorder.nodes.count",
        case,
        case.tree.root_node().preorder().nodes().count(),
        case.nodes
    );
    workload!(
        "scalar_next_preorder",
        case,
        consume(scalar_preorder(&case.tree)),
        case.nodes
    );
    workload!(
        "mainline_cursor",
        case,
        consume(case.native.root_node().preorder()),
        case.nodes
    );

    scan_workloads!(
        "multi_kind",
        case,
        case.tree
            .root_node()
            .all()
            .filter_kind_ids(&case.multiple_kinds),
        case.multiple_kind_matches,
        [nodes, count, fold]
    );
    scan_workloads!(
        "field",
        case,
        case.tree.root_node().all().filter_field_id(case.field),
        case.field_matches,
        [nodes, count]
    );
    scan_workloads!(
        "postorder.field",
        case,
        case.tree
            .root_node()
            .postorder()
            .filter_field_id(case.field),
        case.field_matches,
        [nodes, count]
    );
    scan_workloads!(
        "postorder.kind",
        case,
        case.tree
            .root_node()
            .postorder()
            .filter_kind_ids(&case.kinds),
        case.kind_matches,
        [nodes, count]
    );
    scan_workloads!(
        "postorder.rev.kind",
        case,
        case.tree
            .root_node()
            .postorder()
            .rev()
            .filter_kind_ids(&case.kinds),
        case.kind_matches,
        [nodes]
    );
    workload!(
        "field.scalar",
        case,
        consume(scalar_preorder(&case.tree).filter(|node| node.field_id() == case.field)),
        case.field_matches
    );
    workload!(
        "kind.scalar",
        case,
        consume(scalar_preorder(&case.tree).filter(|node| case.kinds.contains(node.kind_id()))),
        case.kind_matches
    );
    scan_workloads!(
        "supertype",
        case,
        case.tree
            .root_node()
            .all()
            .filter_supertype_id(case.supertype),
        case.supertype_matches,
        [nodes, count]
    );
    scan_workloads!(
        "flags",
        case,
        case.tree
            .root_node()
            .all()
            .filter_extra(false)
            .filter_missing(false),
        case.flags_matches,
        [count]
    );
    scan_workloads!(
        "combined",
        case,
        case.tree
            .root_node()
            .all()
            .filter_kind_ids(&case.kinds)
            .filter_field_id(case.field)
            .filter_extra(false)
            .filter_missing(false),
        case.combined_matches,
        [count]
    );

    scan_workloads!(
        "range",
        case,
        case.tree
            .root_node()
            .all()
            .overlapping_bytes(case.range.clone()),
        case.range_matches,
        [nodes, count, fold, reverse_nodes]
    );
    scan_workloads!(
        "point_range",
        case,
        case.tree
            .root_node()
            .all()
            .overlapping_points(case.point_range.clone()),
        case.range_matches,
        [nodes, count, fold, reverse_nodes]
    );
    workload!(
        "range.scalar",
        case,
        consume(scalar_preorder(&case.tree).filter(|&node| overlaps(node, &case.range))),
        case.range_matches
    );
    workload!(
        "point_range.scalar",
        case,
        consume(
            scalar_preorder(&case.tree).filter(|&node| overlaps_points(node, &case.point_range))
        ),
        case.range_matches
    );
    scan_workloads!(
        "within",
        case,
        case.tree.root_node().all().within_bytes(case.range.clone()),
        case.within_matches,
        [nodes, count, fold]
    );
    scan_workloads!(
        "starting_in",
        case,
        case.tree
            .root_node()
            .all()
            .starting_in_bytes(case.range.clone()),
        case.starting_in_matches,
        [nodes, count, fold]
    );
    scan_workloads!(
        "starting_at",
        case,
        case.tree
            .root_node()
            .all()
            .starting_at_byte(case.range.start),
        case.starting_at_matches,
        [nodes, count, fold]
    );
    scan_workloads!(
        "point_within",
        case,
        case.tree
            .root_node()
            .all()
            .within_points(case.point_range.clone()),
        case.within_matches,
        [nodes, count, fold]
    );
    scan_workloads!(
        "point_starting_in",
        case,
        case.tree
            .root_node()
            .all()
            .starting_in_points(case.point_range.clone()),
        case.starting_in_matches,
        [nodes, count, fold]
    );
    scan_workloads!(
        "point_starting_at",
        case,
        case.tree
            .root_node()
            .all()
            .starting_at_point(case.point_range.start),
        case.starting_at_matches,
        [nodes, count, fold]
    );

    // Keep each pipeline's static specialization and scalar oracle together.
    macro_rules! kind_workloads {
        ($name:literal, $length:literal, $case:ident, $ids:ident, $scan:expr, $expected:expr, $node:ident, $predicate:expr, [$($consumer:ident),+]) => {
            for $case in cases {
                let kinds = &$case.sized_kind_sets[($length as usize).ilog2() as usize];
                let expected = scalar_preorder(&$case.tree)
                    .filter(|$node| kinds.contains($node.kind_id()) && $predicate)
                    .collect::<Vec<_>>();
                let $ids = fixed_kinds::<$length>($case);
                assert_eq!($scan.nodes().collect::<Vec<_>>(), expected, $name);
                let $ids = kinds;
                assert_eq!($scan.nodes().collect::<Vec<_>>(), expected, $name);
            }
            scan_workloads!(concat!($name, "fixed_", $length), $case,
                { let $ids = fixed_kinds::<$length>($case); $scan }, $expected, [$($consumer),+]);
            scan_workloads!(concat!($name, "dynamic_", $length), $case,
                { let $ids = &$case.sized_kind_sets[($length as usize).ilog2() as usize]; $scan }, $expected, [$($consumer),+]);
        };
    }
    macro_rules! range_kind_workloads {
        ($name:literal, $method:ident, $value:ident, $matches:ident, $length:literal, $node:ident, $case:ident, $predicate:expr) => {
            kind_workloads!(
                $name,
                $length,
                $case,
                ids,
                $case
                    .tree
                    .root_node()
                    .all()
                    .$method($case.$value.clone())
                    .filter_kind_ids(ids),
                $case.$matches[($length as usize).ilog2() as usize],
                $node,
                $predicate,
                [nodes, count]
            );
        };
    }
    macro_rules! range_kind_sizes {
        ($($length:literal),+) => {$(
            range_kind_workloads!(
                "range.",
                overlapping_bytes,
                range,
                range_kind_matches,
                $length,
                node,
                case,
                overlaps(*node, &case.range)
            );
            range_kind_workloads!(
                "point_range.",
                overlapping_points,
                point_range,
                range_kind_matches,
                $length,
                node,
                case,
                overlaps_points(*node, &case.point_range)
            );
            range_kind_workloads!(
                "within.",
                within_bytes,
                range,
                within_kind_matches,
                $length,
                node,
                case,
                case.range.start <= node.start_byte() && node.end_byte() <= case.range.end
            );
            range_kind_workloads!(
                "point_within.",
                within_points,
                point_range,
                within_kind_matches,
                $length,
                node,
                case,
                case.point_range.start <= node.start_position()
                    && node.end_position() <= case.point_range.end
            );
        )+};
    }
    range_kind_sizes!(1, 4);
    range_kind_workloads!(
        "range.",
        overlapping_bytes,
        range,
        range_kind_matches,
        8,
        node,
        case,
        overlaps(*node, &case.range)
    );
    macro_rules! combined_kind_sizes {
        ($($length:literal),+) => {$(
            kind_workloads!(
                "field.",
                $length,
                case,
                ids,
                case.tree
                    .root_node()
                    .all()
                    .filter_field_id(case.field)
                    .filter_kind_ids(ids),
                case.field_kind_matches[($length as usize).ilog2() as usize],
                node,
                node.field_id() == case.field,
                [nodes, count]
            );
            kind_workloads!(
                "kind_field.",
                $length,
                case,
                ids,
                case.tree
                    .root_node()
                    .all()
                    .filter_kind_ids(ids)
                    .filter_field_id(case.field),
                case.field_kind_matches[($length as usize).ilog2() as usize],
                node,
                node.field_id() == case.field,
                [nodes, count]
            );
            kind_workloads!(
                "flags.",
                $length,
                case,
                ids,
                case.tree
                    .root_node()
                    .all()
                    .filter_extra(false)
                    .filter_missing(false)
                    .filter_kind_ids(ids),
                case.flags_kind_matches[($length as usize).ilog2() as usize],
                node,
                !node.is_extra() && !node.is_missing(),
                [nodes, count]
            );
            kind_workloads!(
                "range_field.",
                $length,
                case,
                ids,
                case.tree
                    .root_node()
                    .all()
                    .overlapping_bytes(case.range.clone())
                    .filter_field_id(case.field)
                    .filter_kind_ids(ids),
                case.range_field_kind_matches[($length as usize).ilog2() as usize],
                node,
                overlaps(*node, &case.range) && node.field_id() == case.field,
                [nodes, count]
            );
            kind_workloads!(
                "range_kind_field.",
                $length,
                case,
                ids,
                case.tree
                    .root_node()
                    .all()
                    .overlapping_bytes(case.range.clone())
                    .filter_kind_ids(ids)
                    .filter_field_id(case.field),
                case.range_field_kind_matches[($length as usize).ilog2() as usize],
                node,
                overlaps(*node, &case.range) && node.field_id() == case.field,
                [nodes, count]
            );
            kind_workloads!(
                "intersection.",
                $length,
                case,
                ids,
                case.tree
                    .root_node()
                    .all()
                    .filter_kind_ids(ids)
                    .filter_kind_ids(&case.intersecting_kinds),
                case.intersection_matches[($length as usize).ilog2() as usize],
                node,
                case.intersecting_kinds.contains(node.kind_id()),
                [nodes, count]
            );
            kind_workloads!(
                "intersection_reverse.",
                $length,
                case,
                ids,
                case.tree
                    .root_node()
                    .all()
                    .filter_kind_ids(&case.intersecting_kinds)
                    .filter_kind_ids(ids),
                case.intersection_matches[($length as usize).ilog2() as usize],
                node,
                case.intersecting_kinds.contains(node.kind_id()),
                [nodes, count]
            );
        )+};
    }
    combined_kind_sizes!(2, 4, 8, 16);
    macro_rules! kind_sizes {
        ($($length:literal),+) => {$(
            kind_workloads!(
                "",
                $length,
                case,
                ids,
                case.tree.root_node().all().filter_kind_ids(ids),
                case.sized_kind_matches[($length as usize).ilog2() as usize],
                _node,
                true,
                [nodes, count, fold, reverse_nodes]
            );
            for case in cases {
                let kinds = &case.sized_kind_sets[($length as usize).ilog2() as usize];
                let mut expected = scalar_preorder(&case.tree)
                    .filter(|node| kinds.contains(node.kind_id()))
                    .collect::<Vec<_>>();
                expected.reverse();
                assert_eq!(
                    case.tree
                        .root_node()
                        .all()
                        .filter_kind_ids(fixed_kinds::<$length>(case))
                        .rev()
                        .nodes()
                        .collect::<Vec<_>>(),
                    expected
                );
                assert_eq!(
                    case.tree
                        .root_node()
                        .all()
                        .filter_kind_ids(kinds)
                        .rev()
                        .nodes()
                        .collect::<Vec<_>>(),
                    expected
                );
            }
        )+};
    }
    kind_sizes!(1, 2, 4, 8, 16);
    macro_rules! field_sizes {
        ($($length:literal),+) => {$(
            scan_workloads!(
                concat!("fixed_field_", $length),
                case,
                case.tree
                    .root_node()
                    .all()
                    .filter_field_ids(fixed_fields::<$length>(case)),
                case.sized_field_matches[($length as usize).ilog2() as usize],
                [nodes, count]
            );
            scan_workloads!(
                concat!("dynamic_field_", $length),
                case,
                case.tree
                    .root_node()
                    .all()
                    .filter_field_ids(&case.sized_field_sets[($length as usize).ilog2() as usize]),
                case.sized_field_matches[($length as usize).ilog2() as usize],
                [nodes, count]
            );
            workload!(
                concat!("scalar_field_", $length, ".nodes"),
                case,
                consume(
                    scalar_preorder(&case.tree)
                        .filter(|node| fixed_fields::<$length>(case).contains(&node.field_id()))
                ),
                case.sized_field_matches[($length as usize).ilog2() as usize]
            );
        )+};
    }
    field_sizes!(1, 2, 4);
    workloads
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
    let mut cursor = root.walk();
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
    macro_rules! validate_range {
        ($byte_method:ident, $point_method:ident, $byte_value:expr, $point_value:expr, $predicate:expr) => {
            let expected = preorder
                .iter()
                .copied()
                .filter($predicate)
                .collect::<Vec<_>>();
            assert_eq!(
                root.all()
                    .$byte_method($byte_value)
                    .nodes()
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(
                root.all()
                    .$point_method($point_value)
                    .nodes()
                    .collect::<Vec<_>>(),
                expected
            );
        };
    }
    validate_range!(
        within_bytes,
        within_points,
        case.range.clone(),
        case.point_range.clone(),
        |node: &Node<'_>| case.range.start <= node.start_byte()
            && node.end_byte() <= case.range.end
    );
    validate_range!(
        starting_in_bytes,
        starting_in_points,
        case.range.clone(),
        case.point_range.clone(),
        |node: &Node<'_>| case.range.contains(&node.start_byte())
    );
    validate_range!(
        starting_at_byte,
        starting_at_point,
        case.range.start,
        case.point_range.start,
        |node: &Node<'_>| node.start_byte() == case.range.start
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
        let grammar = Language::new(language)?;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(language)?;
        let native = parse(&mut parser, &source, Duration::from_secs(10))?;
        let tree = Forest::pack_with_options(
            &grammar,
            &native,
            PackOptions {
                symbol_presence: &|_| !arguments.no_symbol_index,
                ..Default::default()
            },
        )?;
        let mut frequencies = BTreeMap::new();
        let mut present_kinds = std::collections::BTreeSet::new();
        let mut fields = BTreeMap::new();
        let mut nodes = 0usize;
        for node in scalar_preorder(&tree) {
            nodes += 1;
            present_kinds.insert(node.kind_id());
            if node.field_id() != FieldSelection::default() {
                *fields.entry(node.field_id()).or_insert(0usize) += 1;
            }
            if node.is_named() {
                *frequencies.entry(node.kind_id()).or_insert(0usize) += 1;
            }
        }
        // This bound fits sparse entries at every supported group size, keeping
        // the selected IDs identical in the 16/32/64-slot comparison.
        let sparse_kind_limit = nodes.div_ceil(64 * 32);
        let mut ranked_kinds = frequencies
            .iter()
            .filter(|&(_, count)| {
                !matches!(arguments.kind_selection, KindSelection::Sparse)
                    || *count <= sparse_kind_limit
            })
            .collect::<Vec<_>>();
        match arguments.kind_selection {
            KindSelection::Frequent | KindSelection::Sparse => {
                ranked_kinds.sort_by_key(|&(&kind, &count)| (std::cmp::Reverse(count), kind))
            }
            _ => ranked_kinds.sort_by_key(|&(&kind, &count)| (count, kind)),
        }
        let selected_kinds = if matches!(arguments.kind_selection, KindSelection::Absent) {
            let mut absent = (0..language.node_kind_count())
                .filter_map(|kind| u16::try_from(kind).ok().map(KindId::from_raw))
                .filter(|kind| !present_kinds.contains(kind))
                .collect::<Vec<_>>();
            absent.sort_by_key(|&kind| (!language.node_kind_is_named(u16::from(kind)), kind));
            absent
        } else {
            ranked_kinds.into_iter().map(|(&kind, _)| kind).collect()
        };
        ensure!(
            !selected_kinds.is_empty(),
            "no IDs for the selected kind workload: {}",
            input.path
        );
        let kind = selected_kinds[0];
        let kinds = KindSet::new([kind]);
        let selected_kind_ids =
            std::array::from_fn(|index| selected_kinds.get(index).copied().unwrap_or(kind));
        let sized_kind_sets = std::array::from_fn(|index| {
            KindSet::new(selected_kind_ids[..1 << index].iter().copied())
        });
        let mut sized_kind_matches = [0; 5];
        for node in scalar_preorder(&tree) {
            for (count, kinds) in sized_kind_matches.iter_mut().zip(&sized_kind_sets) {
                *count += usize::from(kinds.contains(node.kind_id()));
            }
        }
        let intersecting_kinds = KindSet::new(selected_kind_ids.iter().copied().step_by(2));
        let multiple_kinds = KindSet::new(selected_kinds.into_iter().take(arguments.kind_count));
        let multiple_kind_matches = scalar_preorder(&tree)
            .filter(|node| multiple_kinds.contains(node.kind_id()))
            .count();
        let field = fields
            .iter()
            .max_by_key(|&(field, count)| (*count, std::cmp::Reverse(*field)))
            .map_or(FieldSelection::default(), |(&field, _)| field);
        let mut ranked_fields = fields.iter().collect::<Vec<_>>();
        ranked_fields.sort_by_key(|&(&field, &count)| (std::cmp::Reverse(count), field));
        let frequent_field_ids = std::array::from_fn(|index| {
            ranked_fields.get(index).map_or(field, |&(&field, _)| field)
        });
        let sized_field_sets = std::array::from_fn(|index| {
            FieldSet::new(frequent_field_ids[..1 << index].iter().copied())
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
        let within_matches = scalar_preorder(&tree)
            .filter(|node| range.start <= node.start_byte() && node.end_byte() <= range.end)
            .count();
        let starting_in_matches = scalar_preorder(&tree)
            .filter(|node| range.contains(&node.start_byte()))
            .count();
        let starting_at_matches = scalar_preorder(&tree)
            .filter(|node| node.start_byte() == range.start)
            .count();
        let range_matches = scalar_preorder(&tree)
            .filter(|&node| overlaps(node, &range))
            .count();
        let mut range_kind_matches = [0; 5];
        let mut within_kind_matches = [0; 5];
        let mut field_kind_matches = [0; 5];
        let mut flags_kind_matches = [0; 5];
        let mut range_field_kind_matches = [0; 5];
        let mut intersection_matches = [0; 5];
        for node in scalar_preorder(&tree) {
            let overlapping = overlaps(node, &range);
            let within = range.start <= node.start_byte() && node.end_byte() <= range.end;
            let selected_field = node.field_id() == field;
            for (index, kinds) in sized_kind_sets.iter().enumerate() {
                if kinds.contains(node.kind_id()) {
                    range_kind_matches[index] += usize::from(overlapping);
                    within_kind_matches[index] += usize::from(within);
                    field_kind_matches[index] += usize::from(selected_field);
                    flags_kind_matches[index] +=
                        usize::from(!node.is_extra() && !node.is_missing());
                    range_field_kind_matches[index] += usize::from(overlapping && selected_field);
                    intersection_matches[index] +=
                        usize::from(intersecting_kinds.contains(node.kind_id()));
                }
            }
        }
        let supertype =
            GrammarId::from_raw(language.supertypes().first().copied().unwrap_or(u16::MAX));
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
            "nodes": nodes, "groups": tree.group_count(), "slots": tree.slot_count(), "kind_id": u16::from(kind),
            "kind": language.node_kind_for_id(u16::from(kind)), "kind_matches": kind_matches,
            "multiple_kind_matches": multiple_kind_matches,
            "field_id": raw_field(field), "field_matches": field_matches,
            "range": [range.start, range.end], "range_matches": range_matches,
            "range_kind_matches": range_kind_matches, "within_kind_matches": within_kind_matches,
            "field_kind_matches": field_kind_matches,
            "flags_kind_matches": flags_kind_matches,
            "range_field_kind_matches": range_field_kind_matches,
            "intersecting_kind_ids": selected_kind_ids.iter().copied().step_by(2).map(u16::from).collect::<Vec<_>>(),
            "intersection_matches": intersection_matches,
            "point_range": [
                [point_range.start.row, point_range.start.column],
                [point_range.end.row, point_range.end.column],
            ],
            "supertype_id": u16::from(supertype), "supertype_count": language.supertypes().len(),
            "supertype_matches": supertype_matches,
            "flags_matches": flags_matches, "combined_matches": combined_matches,
            "selected_kind_ids": selected_kind_ids.map(u16::from),
            "selected_kind_frequencies": selected_kind_ids.map(|kind| frequencies.get(&kind).copied().unwrap_or(0)),
            "sparse_kind_limit": sparse_kind_limit, "sized_kind_matches": sized_kind_matches,
            "frequent_field_ids": frequent_field_ids.map(raw_field), "sized_field_matches": sized_field_matches,
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
            range_kind_matches,
            within_matches,
            within_kind_matches,
            field_kind_matches,
            flags_kind_matches,
            range_field_kind_matches,
            intersecting_kinds,
            intersection_matches,
            starting_in_matches,
            starting_at_matches,
            supertype,
            supertype_matches,
            flags_matches,
            combined_matches,
            selected_kind_ids,
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
    let mut workloads = workloads(&cases);
    for name in &arguments.workload {
        ensure!(
            workloads.iter().any(|workload| workload.name == name),
            "unknown workload: {name}"
        );
    }
    if !arguments.workload.is_empty() {
        workloads.retain(|workload| {
            arguments
                .workload
                .iter()
                .any(|selected| selected == workload.name)
        });
    }
    if arguments.reverse_workloads {
        workloads.reverse();
    }
    let mut results = Vec::new();
    for workload in &workloads {
        let name = workload.name;
        let operation = workload.operation;
        let expected = cases.iter().map(workload.expected).sum::<usize>();
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
            let operation = workloads[index].operation;
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
        "squatter_backend": squatter_bench::BACKEND,
        "arguments": arguments, "inputs": descriptions, "results": results,
        "representation_id": tree_squatter::representation_id(),
        "grammar_sha256": grammars.iter().map(|(name, grammar)| (name, &grammar.sha256)).collect::<BTreeMap<_, _>>(),
        "cpuinfo": fs::read_to_string("/proc/cpuinfo").ok(),
        "contract": "release build; cyclic corpus; scan construction included; parsing, packing and validation excluded; black_box each enumerated node; count consumes only aggregate; input nodes/s includes nodes skipped by group/range operations; median wall-clock throughput; workload order rotates each sample",
    });
    fs::write(&arguments.output, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}
