//! Uninlined scan consumers for inspecting optimized code generation.
mod support;

use std::{hint::black_box, ops::Range};
use tree_sitter::Point;
use tree_squatter::{FieldId, GrammarId, KindId, KindSet, Language, Node, SlotIx, Tree};

use support::{json_language, parse_native};

const SOURCE: &str = r#"{"a": [1, 2], "b": {"c": 3}, "d": 4}"#;

fn fixture() -> (Language, Tree) {
    let language = json_language();
    let grammar = Language::new(&language).unwrap();
    let native = parse_native(&language, SOURCE);
    let tree = Tree::pack(&grammar, &native).unwrap();
    (grammar, tree)
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
            .map(|node| u64::from(node.slot().get_raw()))
            .sum()
    }
    #[inline(never)]
    pub fn point_range_slots(root: Node<'_>, range: Range<Point>) -> u64 {
        root.all()
            .overlapping_points(range)
            .nodes()
            .map(|node| u64::from(node.slot().get_raw()))
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
    pub fn supertype_count(root: Node<'_>, supertype: GrammarId) -> usize {
        root.all().filter_supertype_id(supertype).count()
    }
    #[inline(never)]
    pub fn preorder_slots(root: Node<'_>) -> u64 {
        root.preorder()
            .nodes()
            .map(|node| u64::from(node.slot().get_raw()))
            .sum()
    }
    #[inline(never)]
    pub fn reverse_preorder_slots(root: Node<'_>) -> u64 {
        root.preorder()
            .rev()
            .nodes()
            .map(|node| u64::from(node.slot().get_raw()))
            .sum()
    }
    #[inline(never)]
    pub fn postorder_slots(root: Node<'_>) -> u64 {
        root.postorder()
            .nodes()
            .map(|node| u64::from(node.slot().get_raw()))
            .sum()
    }
    #[inline(never)]
    pub fn reverse_postorder_slots(root: Node<'_>) -> u64 {
        root.postorder()
            .rev()
            .nodes()
            .map(|node| u64::from(node.slot().get_raw()))
            .sum()
    }
    #[inline(never)]
    pub fn grouped_slots(root: Node<'_>) -> u64 {
        root.preorder()
            .groups()
            .map(|group| {
                group
                    .nodes()
                    .map(|node| u64::from(node.slot().get_raw()))
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
fn assembly_patterns_are_callable() {
    let (grammar, tree) = fixture();
    let root = black_box(tree.root_node());
    assert_eq!(patterns::all_count(root), 41);
    assert_eq!(patterns::preorder_count(root), 41);
    assert_eq!(patterns::postorder_count(root), 41);
    assert_eq!(patterns::reverse_postorder_count(root), 41);
    assert_eq!(patterns::nodes_count(root), 41);

    let slots = patterns::preorder_slots(root);
    assert_eq!(patterns::reverse_preorder_slots(root), slots);
    assert_eq!(patterns::postorder_slots(root), slots);
    assert_eq!(patterns::reverse_postorder_slots(root), slots);
    assert_eq!(patterns::grouped_slots(root), slots);

    let number = grammar.kind_id_for_name("number", true).unwrap();
    let numbers = KindSet::new([number]);
    let numbers = black_box(&numbers);
    assert_eq!(patterns::kind_count(root, numbers), 4);
    assert_eq!(patterns::scalar_kind_count(root, numbers), 4);
    assert_eq!(patterns::four_kind_count(root, black_box([number; 4])), 4);
    assert_eq!(patterns::eight_kind_count(root, black_box([number; 8])), 4);
    black_box(patterns::first_kind_slot(root, numbers));
    black_box(patterns::kind_start_bytes(root, numbers));

    let value_field = black_box(Some(grammar.field_id_for_name("value").unwrap()));
    assert_eq!(patterns::field_count(root, value_field), 4);
    assert_eq!(
        patterns::two_field_count(
            root,
            black_box([Some(grammar.field_id_for_name("key").unwrap()), value_field]),
        ),
        8
    );
    let range = black_box(0..SOURCE.len());
    assert_eq!(patterns::range_count(root, range.clone()), 41);
    assert_eq!(patterns::range_slots(root, range.clone()), slots);
    assert_eq!(
        patterns::point_range_slots(
            root,
            black_box(Point::new(0, range.start)..Point::new(0, range.end)),
        ),
        slots
    );
    assert_eq!(
        patterns::combined_count(root, range, numbers, value_field),
        2
    );
    assert_eq!(
        patterns::supertype_count(root, black_box(GrammarId::from_raw(0))),
        0
    );
}
