//! Small scan examples, also used to inspect optimized code generation.
use std::{hint::black_box, ops::Range};
use tree_squatter::{Grammar, KindSet, Node, Tree};

const SOURCE: &str = r#"{"a": [1, 2], "b": {"c": 3}, "d": 4}"#;

fn fixture() -> (tree_sitter::Language, Tree) {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse(SOURCE, None).unwrap();
    let tree = Tree::pack(&Grammar::new(&language).unwrap(), &native).unwrap();
    (language, tree)
}

fn texts<'tree>(nodes: impl IntoIterator<Item = Node<'tree>>) -> Vec<&'static str> {
    nodes
        .into_iter()
        .map(|node| node.utf8_text(SOURCE.as_bytes()).unwrap())
        .collect()
}

#[test]
fn scan_patterns() {
    let (language, tree) = fixture();
    let root = tree.root_node();
    let numbers = KindSet::new([language.id_for_node_kind("number", true)]);
    let containers_and_numbers =
        KindSet::new(["pair", "array", "number"].map(|name| language.id_for_node_kind(name, true)));
    let value_field = language.field_id_for_name("value").unwrap().get();

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
                .nodes()
                .rev()
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
    let key_field = language.field_id_for_name("key").unwrap().get();
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
        .filter_kind_ids(&KindSet::new([language.id_for_node_kind("array", true)]))
        .nodes()
        .next()
        .unwrap();
    assert_eq!(
        texts(array.preorder().filter_kind_ids(&numbers)),
        ["1", "2"]
    );

    let mut both_ends = root.postorder().filter_kind_ids(&numbers).nodes();
    assert_eq!(texts(both_ends.next()), ["1"]);
    assert_eq!(texts(both_ends.next_back()), ["4"]);
    assert_eq!(both_ends.count(), 2);

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
    pub fn four_kind_count(root: Node<'_>, kinds: [u16; 4]) -> usize {
        root.all().filter_kind_ids(kinds).count()
    }
    #[inline(never)]
    pub fn eight_kind_count(root: Node<'_>, kinds: [u16; 8]) -> usize {
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
    pub fn field_count(root: Node<'_>, field: u16) -> usize {
        root.all().filter_field_id(field).count()
    }
    #[inline(never)]
    pub fn two_field_count(root: Node<'_>, fields: [u16; 2]) -> usize {
        root.all().filter_field_ids(fields).count()
    }
    #[inline(never)]
    pub fn range_count(root: Node<'_>, range: Range<usize>) -> usize {
        root.all().overlapping_bytes(range).count()
    }
    #[inline(never)]
    pub fn combined_count(
        root: Node<'_>,
        range: Range<usize>,
        kinds: &KindSet,
        field: u16,
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
    pub fn supertype_count(root: Node<'_>, supertype: u16) -> usize {
        root.all().filter_supertype_id(supertype).count()
    }
    #[inline(never)]
    pub fn preorder_slots(root: Node<'_>) -> u64 {
        root.preorder()
            .nodes()
            .map(|node| u64::from(node.slot()))
            .sum()
    }
    #[inline(never)]
    pub fn reverse_preorder_slots(root: Node<'_>) -> u64 {
        root.preorder()
            .rev()
            .nodes()
            .map(|node| u64::from(node.slot()))
            .sum()
    }
    #[inline(never)]
    pub fn postorder_slots(root: Node<'_>) -> u64 {
        root.postorder()
            .nodes()
            .map(|node| u64::from(node.slot()))
            .sum()
    }
    #[inline(never)]
    pub fn reverse_postorder_slots(root: Node<'_>) -> u64 {
        root.postorder()
            .rev()
            .nodes()
            .map(|node| u64::from(node.slot()))
            .sum()
    }
    #[inline(never)]
    pub fn grouped_slots(root: Node<'_>) -> u64 {
        root.preorder()
            .groups()
            .map(|group| {
                group
                    .nodes()
                    .map(|node| u64::from(node.slot()))
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
    pub fn first_kind_slot(root: Node<'_>, kinds: &KindSet) -> Option<u32> {
        root.preorder()
            .filter_kind_ids(kinds)
            .nodes()
            .next()
            .map(|node| node.slot())
    }
}

#[test]
fn assembly_patterns_match_examples() {
    let (language, tree) = fixture();
    let root = black_box(tree.root_node());
    // Scalar navigation supplies an independent reference for the reductions.
    let nodes = std::iter::successors(Some(root), |node| node.next_preorder()).collect::<Vec<_>>();
    assert_eq!(nodes.len(), 41);
    assert_eq!(patterns::all_count(root), 41);
    assert_eq!(patterns::preorder_count(root), 41);
    assert_eq!(patterns::postorder_count(root), 41);
    assert_eq!(patterns::reverse_postorder_count(root), 41);
    assert_eq!(patterns::nodes_count(root), 41);

    let slots = nodes.iter().map(|node| u64::from(node.slot())).sum::<u64>();
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
                .map(|name| language.id_for_node_kind(name, true)),
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

    let fixed =
        ["number", "array", "pair", "string"].map(|name| language.id_for_node_kind(name, true));
    let expected = nodes
        .iter()
        .filter(|node| fixed.contains(&node.kind_id()))
        .count();
    assert_eq!(patterns::four_kind_count(root, black_box(fixed)), expected);
    assert_eq!(
        patterns::eight_kind_count(
            root,
            black_box([
                fixed[0], fixed[1], fixed[2], fixed[3], 32768, 32769, fixed[0], fixed[1]
            ])
        ),
        expected
    );
    let numbers = KindSet::new([language.id_for_node_kind("number", true)]);
    let numbers = black_box(&numbers);
    let value_field = black_box(language.field_id_for_name("value").unwrap().get());
    assert_eq!(patterns::field_count(root, value_field), 4);
    assert_eq!(
        patterns::two_field_count(
            root,
            black_box([
                language.field_id_for_name("key").unwrap().get(),
                value_field
            ])
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

    for &supertype in language.supertypes() {
        let supertype = black_box(supertype);
        let expected = nodes
            .iter()
            .filter(|node| node.has_supertype(supertype))
            .count();
        assert!(expected > 0);
        assert_eq!(patterns::supertype_count(root, supertype), expected);
    }
}
