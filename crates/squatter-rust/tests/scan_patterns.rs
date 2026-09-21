//! Small scan examples, also used to inspect optimized code generation.
#[path = "../../squatter/tests/support/mod.rs"]
mod support;

use std::{hint::black_box, ops::Range};
use tree_sitter::Point;
use tree_squatter_rust::{FieldId, Grammar, GrammarKindId, KindId, KindSet, Node, SlotIx, Tree};

use support::{json_language, parse_native};

const SOURCE: &str = r#"{"a": [1, 2], "b": {"c": 3}, "d": 4}"#;

fn fixture() -> (Grammar, Tree) {
    let language = json_language();
    let grammar = Grammar::new(&language).unwrap();
    let native = parse_native(&language, SOURCE);
    let tree = Tree::pack(&grammar, &native).unwrap();
    (grammar, tree)
}

fn texts<'tree>(nodes: impl IntoIterator<Item = Node<'tree>>) -> Vec<&'static str> {
    nodes
        .into_iter()
        .map(|node| node.utf8_text(SOURCE.as_bytes()).unwrap())
        .collect()
}

#[test]
fn scan_patterns() {
    let (grammar, tree) = fixture();
    let root = tree.root_node();
    let numbers = KindSet::new([grammar.kind_id_for_name("number", true).unwrap()]);
    let containers_and_numbers = KindSet::new(
        ["pair", "array", "number"].map(|name| grammar.kind_id_for_name(name, true).unwrap()),
    );
    let value_field = Some(grammar.field_id_for_name("value").unwrap());

    let preorder = [
        r#""a": [1, 2]"#,
        "[1, 2]",
        "1",
        "2",
        r#""b": {"c": 3}"#,
        r#""c": 3"#,
        "3",
        r#""d": 4"#,
        "4",
    ];
    let postorder = [
        "1",
        "2",
        "[1, 2]",
        r#""a": [1, 2]"#,
        "3",
        r#""c": 3"#,
        r#""b": {"c": 3}"#,
        "4",
        r#""d": 4"#,
    ];
    assert_eq!(
        texts(root.preorder().filter_kind_ids(&containers_and_numbers)),
        preorder
    );
    assert_eq!(
        texts(root.postorder().filter_kind_ids(&containers_and_numbers)),
        postorder
    );
    assert_eq!(
        texts(
            root.preorder()
                .rev()
                .filter_kind_ids(&containers_and_numbers)
        ),
        preorder.into_iter().rev().collect::<Vec<_>>()
    );
    assert_eq!(
        texts(
            root.postorder()
                .filter_kind_ids(&containers_and_numbers)
                .rev()
                .nodes()
        ),
        postorder.into_iter().rev().collect::<Vec<_>>()
    );

    // all() promises membership, not a particular traversal order.
    let mut all_numbers = texts(root.all().filter_kind_ids(&numbers));
    all_numbers.sort_unstable();
    assert_eq!(all_numbers, ["1", "2", "3", "4"]);
    assert_eq!(
        texts(root.preorder().filter_field_id(value_field)),
        ["[1, 2]", r#"{"c": 3}"#, "3", "4"]
    );
    let key_field = Some(grammar.field_id_for_name("key").unwrap());
    assert_eq!(
        texts(root.preorder().filter_field_ids([key_field, value_field])),
        [
            r#""a""#,
            "[1, 2]",
            r#""b""#,
            r#"{"c": 3}"#,
            r#""c""#,
            "3",
            r#""d""#,
            "4"
        ]
    );
    assert_eq!(
        texts(
            root.preorder()
                .filter_kind_ids(&numbers)
                .filter_field_id(value_field)
        ),
        ["3", "4"]
    );

    // Overlap includes crossing ancestors, not just nodes contained in the range.
    let third_number = SOURCE.find('3').unwrap();
    assert_eq!(
        texts(
            root.preorder()
                .overlapping_bytes(third_number..third_number + 1)
                .filter_kind_ids(&containers_and_numbers)
        ),
        [r#""b": {"c": 3}"#, r#""c": 3"#, "3"]
    );
    assert_eq!(
        root.all()
            .overlapping_bytes(third_number..third_number)
            .count(),
        0
    );

    let array = root
        .preorder()
        .filter_kind_ids(&KindSet::new([grammar
            .kind_id_for_name("array", true)
            .unwrap()]))
        .nodes()
        .next()
        .unwrap();
    assert_eq!(
        texts(array.preorder().filter_kind_ids(&numbers)),
        ["1", "2"]
    );

    let mut backwards = root.postorder().filter_kind_ids(&numbers).rev().nodes();
    assert_eq!(texts(backwards.next()), ["4"]);
    assert_eq!(texts(backwards.next()), ["3"]);
    assert_eq!(backwards.count(), 2);

    let grouped_numbers = root.preorder().filter_kind_ids(&numbers).groups();
    let mut matching_slots = 0;
    let mut grouped_texts = Vec::new();
    for group in grouped_numbers {
        matching_slots += group.matches().count_ones();
        grouped_texts.extend(texts(group.nodes()));
    }
    assert_eq!(matching_slots, 4);
    assert_eq!(grouped_texts, ["1", "2", "3", "4"]);
}

// Keeping only the consumer boundaries uninlined makes them easy to find in ASM.
// The scan adapters can inline normally; inputs are opaque at the test call sites.
mod patterns {
    use super::*;

    #[inline(never)]
    pub fn all_count(root: Node<'_>) -> usize {
        root.all().count()
    }
    #[inline(never)]
    pub fn preorder_count(root: Node<'_>) -> usize {
        root.preorder().count()
    }
    #[inline(never)]
    pub fn postorder_count(root: Node<'_>) -> usize {
        root.postorder().count()
    }
    #[inline(never)]
    pub fn reverse_postorder_count(root: Node<'_>) -> usize {
        root.postorder().rev().count()
    }
    #[inline(never)]
    pub fn nodes_count(root: Node<'_>) -> usize {
        root.all().nodes().count()
    }
    #[inline(never)]
    pub fn kind_count(root: Node<'_>, kinds: &KindSet) -> usize {
        root.all().filter_kind_ids(kinds).count()
    }
    #[inline(never)]
    pub fn four_kind_count(root: Node<'_>, kinds: [KindId; 4]) -> usize {
        root.all().filter_kind_ids(kinds).count()
    }
    #[inline(never)]
    pub fn eight_kind_count(root: Node<'_>, kinds: [KindId; 8]) -> usize {
        root.all().filter_kind_ids(kinds).count()
    }
    #[inline(never)]
    pub fn scalar_kind_count(root: Node<'_>, kinds: &KindSet) -> usize {
        root.all()
            .nodes()
            .filter(|node| kinds.contains(node.kind_id()))
            .count()
    }
    #[inline(never)]
    pub fn field_count(root: Node<'_>, field: Option<FieldId>) -> usize {
        root.all().filter_field_id(field).count()
    }
    #[inline(never)]
    pub fn two_field_count(root: Node<'_>, fields: [Option<FieldId>; 2]) -> usize {
        root.all().filter_field_ids(fields).count()
    }
    #[inline(never)]
    pub fn range_count(root: Node<'_>, range: Range<usize>) -> usize {
        root.all().overlapping_bytes(range).count()
    }
    #[inline(never)]
    pub fn range_slots(root: Node<'_>, range: Range<usize>) -> u64 {
        root.all()
            .overlapping_bytes(range)
            .nodes()
            .map(|node| u64::from(node.slot().get()))
            .sum()
    }
    #[inline(never)]
    pub fn point_range_slots(root: Node<'_>, range: Range<Point>) -> u64 {
        root.all()
            .overlapping_points(range)
            .nodes()
            .map(|node| u64::from(node.slot().get()))
            .sum()
    }
    #[inline(never)]
    pub fn combined_count(
        root: Node<'_>,
        range: Range<usize>,
        kinds: &KindSet,
        field: Option<FieldId>,
    ) -> usize {
        root.all()
            .overlapping_bytes(range)
            .filter_kind_ids(kinds)
            .filter_field_id(field)
            .filter_extra(false)
            .filter_missing(false)
            .count()
    }
    #[inline(never)]
    pub fn supertype_count(root: Node<'_>, supertype: GrammarKindId) -> usize {
        root.all().filter_supertype_id(supertype).count()
    }
    #[inline(never)]
    pub fn preorder_slots(root: Node<'_>) -> u64 {
        root.preorder()
            .nodes()
            .map(|node| u64::from(node.slot().get()))
            .sum()
    }
    #[inline(never)]
    pub fn reverse_preorder_slots(root: Node<'_>) -> u64 {
        root.preorder()
            .rev()
            .nodes()
            .map(|node| u64::from(node.slot().get()))
            .sum()
    }
    #[inline(never)]
    pub fn postorder_slots(root: Node<'_>) -> u64 {
        root.postorder()
            .nodes()
            .map(|node| u64::from(node.slot().get()))
            .sum()
    }
    #[inline(never)]
    pub fn reverse_postorder_slots(root: Node<'_>) -> u64 {
        root.postorder()
            .rev()
            .nodes()
            .map(|node| u64::from(node.slot().get()))
            .sum()
    }
    #[inline(never)]
    pub fn grouped_slots(root: Node<'_>) -> u64 {
        root.preorder()
            .groups()
            .map(|group| {
                group
                    .nodes()
                    .map(|node| u64::from(node.slot().get()))
                    .sum::<u64>()
            })
            .sum()
    }
    #[inline(never)]
    pub fn kind_start_bytes(root: Node<'_>, kinds: &KindSet) -> usize {
        root.all()
            .filter_kind_ids(kinds)
            .nodes()
            .map(|node| node.start_byte())
            .sum()
    }
    #[inline(never)]
    pub fn first_kind_slot(root: Node<'_>, kinds: &KindSet) -> Option<SlotIx> {
        root.preorder()
            .filter_kind_ids(kinds)
            .nodes()
            .next()
            .map(|node| node.slot())
    }
}

#[test]
fn assembly_patterns_match_examples() {
    let (grammar, tree) = fixture();
    let root = black_box(tree.root_node());
    // Scalar navigation supplies an independent reference for the reductions.
    let nodes = std::iter::successors(Some(root), |node| node.next_preorder()).collect::<Vec<_>>();
    assert_eq!(nodes.len(), 41);
    assert_eq!(patterns::all_count(root), 41);
    assert_eq!(patterns::preorder_count(root), 41);
    assert_eq!(patterns::postorder_count(root), 41);
    assert_eq!(patterns::reverse_postorder_count(root), 41);
    assert_eq!(patterns::nodes_count(root), 41);

    let slots = nodes
        .iter()
        .map(|node| u64::from(node.slot().get()))
        .sum::<u64>();
    assert_eq!(patterns::preorder_slots(root), slots);
    assert_eq!(patterns::reverse_preorder_slots(root), slots);
    assert_eq!(patterns::postorder_slots(root), slots);
    assert_eq!(patterns::reverse_postorder_slots(root), slots);
    assert_eq!(patterns::grouped_slots(root), slots);

    for (names, count) in [
        (vec!["number"], 4),
        (vec!["pair", "array", "number"], 9),
        (vec![], 0),
    ] {
        let kinds = KindSet::new(
            names
                .iter()
                .map(|name| grammar.kind_id_for_name(name, true).unwrap()),
        );
        let kinds = black_box(&kinds);
        assert_eq!(patterns::kind_count(root, kinds), count);
        assert_eq!(patterns::scalar_kind_count(root, kinds), count);
        let matching_nodes = nodes.iter().filter(|node| kinds.contains(node.kind_id()));
        assert_eq!(
            patterns::first_kind_slot(root, kinds),
            matching_nodes.clone().next().map(|node| node.slot())
        );
        assert_eq!(
            patterns::kind_start_bytes(root, kinds),
            matching_nodes.map(|node| node.start_byte()).sum::<usize>()
        );
    }

    let fixed = ["number", "array", "pair", "string"]
        .map(|name| grammar.kind_id_for_name(name, true).unwrap());
    let expected = nodes
        .iter()
        .filter(|node| fixed.contains(&node.kind_id()))
        .count();
    assert_eq!(patterns::four_kind_count(root, black_box(fixed)), expected);
    assert_eq!(
        patterns::eight_kind_count(
            root,
            black_box([
                fixed[0],
                fixed[1],
                fixed[2],
                fixed[3],
                KindId::new(32768),
                KindId::new(32769),
                fixed[0],
                fixed[1]
            ])
        ),
        expected
    );
    let numbers = KindSet::new([grammar.kind_id_for_name("number", true).unwrap()]);
    let numbers = black_box(&numbers);
    let value_field = black_box(Some(grammar.field_id_for_name("value").unwrap()));
    assert_eq!(patterns::field_count(root, value_field), 4);
    assert_eq!(
        patterns::two_field_count(
            root,
            black_box([Some(grammar.field_id_for_name("key").unwrap()), value_field])
        ),
        8
    );
    assert_eq!(
        patterns::combined_count(root, black_box(0..SOURCE.len()), numbers, value_field),
        2
    );
    let third_number = SOURCE.find('3').unwrap();
    assert_eq!(patterns::range_count(root, black_box(0..SOURCE.len())), 41);
    assert_eq!(
        patterns::range_count(root, black_box(third_number..third_number + 1)),
        6
    );
    assert_eq!(patterns::range_count(root, black_box(0..0)), 0);
    for range in [
        0..0,
        0..SOURCE.len(),
        third_number..third_number + 1,
        0..usize::MAX,
    ] {
        let expected = nodes
            .iter()
            .filter(|node| {
                !range.is_empty()
                    && node.start_byte() < range.end
                    && (node.end_byte() > range.start || node.start_byte() >= range.start)
            })
            .map(|node| u64::from(node.slot().get()))
            .sum::<u64>();
        assert_eq!(
            patterns::range_slots(root, black_box(range.clone())),
            expected
        );
        assert_eq!(
            patterns::point_range_slots(
                root,
                black_box(Point::new(0, range.start)..Point::new(0, range.end)),
            ),
            expected,
        );
    }
    assert_eq!(
        patterns::combined_count(
            root,
            black_box(third_number..third_number + 1),
            numbers,
            value_field
        ),
        1
    );
    assert_eq!(
        patterns::combined_count(root, black_box(0..0), numbers, value_field),
        0
    );

    for &supertype in grammar.language().supertypes() {
        let supertype = black_box(GrammarKindId::new(supertype));
        let expected = nodes
            .iter()
            .filter(|node| node.has_supertype(supertype))
            .count();
        assert!(expected > 0);
        assert_eq!(patterns::supertype_count(root, supertype), expected);
    }
}
