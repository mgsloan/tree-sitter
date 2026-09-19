use std::collections::{HashSet, VecDeque};
use tree_squatter::{
    Grammar, IdSet, KindSet, Node, PackOptions, Tree,
    scan::{GroupScan, Scan},
};

type Description = (u16, usize, usize);
fn describe(node: Node<'_>) -> Description {
    (node.kind_id(), node.start_byte(), node.end_byte())
}
fn native_orders(root: tree_sitter::Node<'_>) -> (Vec<Description>, Vec<Description>) {
    fn visit(
        node: tree_sitter::Node<'_>,
        preorder: &mut Vec<Description>,
        postorder: &mut Vec<Description>,
    ) {
        let description = (node.kind_id(), node.start_byte(), node.end_byte());
        preorder.push(description);
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(child, preorder, postorder);
        }
        postorder.push(description);
    }
    let (mut preorder, mut postorder) = (Vec::new(), Vec::new());
    visit(root, &mut preorder, &mut postorder);
    (preorder, postorder)
}
fn json_language() -> tree_sitter::Language {
    unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) }
}
fn parse(
    language: &tree_sitter::Language,
    source: &str,
    options: PackOptions,
) -> (tree_sitter::Tree, Tree) {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(language).unwrap();
    let native = parser.parse(source, None).unwrap();
    let packed =
        Tree::pack_with_options(&Grammar::new(language).unwrap(), &native, options).unwrap();
    (native, packed)
}

fn check_pipeline<'tree, S: GroupScan<'tree>>(
    make: impl Fn() -> Scan<'tree, S>,
    expected: &[Node<'tree>],
) {
    assert_eq!(make().nodes().collect::<Vec<_>>(), expected);
    assert_eq!(make().count(), expected.len());
    assert_eq!(make().rev().count(), expected.len());
    assert_eq!(make().nodes().count(), expected.len());
    assert_eq!(
        make().rev().nodes().collect::<Vec<_>>(),
        expected.iter().rev().copied().collect::<Vec<_>>()
    );
    assert_eq!(
        make().nodes().rev().collect::<Vec<_>>(),
        expected.iter().rev().copied().collect::<Vec<_>>()
    );
    assert_eq!(make().rev().rev().nodes().collect::<Vec<_>>(), expected);
    assert_eq!(
        make()
            .groups()
            .flat_map(|group| group.nodes())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        make()
            .groups()
            .rev()
            .flat_map(|group| group.nodes())
            .collect::<Vec<_>>(),
        expected.iter().rev().copied().collect::<Vec<_>>()
    );

    let mut seen = HashSet::new();
    let mut count = 0;
    for fragment in make().groups() {
        let mask = fragment.matches();
        let group = fragment.group();
        assert!(!mask.is_empty());
        assert_eq!(mask.intersection(group.valid_mask()), mask);
        count += mask.count_ones() as usize;
        for node in fragment.nodes() {
            assert!(seen.insert(node.slot()));
            assert_eq!(group.node(node.slot() - group.first_slot()), Some(node));
        }
        assert_eq!(group.node(u32::MAX), None);
    }
    assert_eq!(count, expected.len());
    for period in [2, 3, 5] {
        let mut nodes = make().nodes();
        let mut remaining: VecDeque<_> = expected.iter().copied().collect();
        let mut index = 0;
        while !remaining.is_empty() {
            if index % period == 0 {
                assert_eq!(nodes.next_back(), remaining.pop_back());
            } else {
                assert_eq!(nodes.next(), remaining.pop_front());
            }
            index += 1;
        }
        for _ in 0..2 {
            assert_eq!(nodes.next(), None);
            assert_eq!(nodes.next_back(), None);
        }
    }
    for consumed in [0, 1, 2, 7, 16, 33] {
        let mut nodes = make().nodes();
        let mut remaining: VecDeque<_> = expected.iter().copied().collect();
        for index in 0..consumed {
            if index % 2 == 0 {
                assert_eq!(nodes.next(), remaining.pop_front());
            } else {
                assert_eq!(nodes.next_back(), remaining.pop_back());
            }
        }
        assert_eq!(nodes.count(), remaining.len());

        for reverse in [false, true] {
            let mut nodes = make().nodes();
            let mut remaining: VecDeque<_> = expected.iter().copied().collect();
            for index in 0..consumed {
                if index % 2 == 0 {
                    assert_eq!(nodes.next(), remaining.pop_front());
                } else {
                    assert_eq!(nodes.next_back(), remaining.pop_back());
                }
            }
            let append = |mut result: Vec<_>, node| {
                result.push(node);
                result
            };
            let actual = if reverse {
                nodes.rfold(Vec::new(), append)
            } else {
                nodes.fold(Vec::new(), append)
            };
            let mut expected = remaining.into_iter().collect::<Vec<_>>();
            if reverse {
                expected.reverse();
            }
            assert_eq!(actual, expected);
        }
    }
}

#[test]
fn orders_subtrees_groups_and_both_ends() {
    let source = format!(
        "{{\"a\": [1, {{\"b\": true}}, null], \"wide\": [{}0]}}",
        "[1,2],".repeat(90)
    );
    let (native, tree) = parse(
        &json_language(),
        &source,
        PackOptions {
            initial_group_capacity: 1,
            ..Default::default()
        },
    );
    let (preorder, postorder) = native_orders(native.root_node());
    assert_eq!(
        tree.root_node()
            .preorder()
            .nodes()
            .map(describe)
            .collect::<Vec<_>>(),
        preorder
    );
    assert_eq!(
        tree.root_node()
            .postorder()
            .nodes()
            .map(describe)
            .collect::<Vec<_>>(),
        postorder
    );
    for root in tree.root_node().node_iterator().unwrap() {
        let expected = root.node_iterator().unwrap().collect::<Vec<_>>();
        check_pipeline(|| root.preorder(), &expected);
        check_pipeline(|| root.all(), &expected);
        let mut cursor = root.walk().unwrap();
        let mut postorder = Vec::new();
        loop {
            while cursor.goto_first_child() {}
            postorder.push(cursor.node());
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    break;
                }
                postorder.push(cursor.node());
            }
            if postorder.last() == Some(&root) {
                break;
            }
        }
        check_pipeline(|| root.postorder(), &postorder);
    }
}

#[allow(clippy::reversed_empty_ranges)] // Intentionally exercise reversed bounds.
fn check_ranges(tree: &Tree, source_len: usize) {
    let all = tree
        .root_node()
        .node_iterator()
        .unwrap()
        .collect::<Vec<_>>();
    let kinds = KindSet::new(all.iter().step_by(3).map(|node| node.kind_id()));
    let roots = all.iter().copied().step_by((all.len() / 15).max(1));
    for root in roots {
        let preorder = root.node_iterator().unwrap().collect::<Vec<_>>();
        let postorder = root.postorder().nodes().collect::<Vec<_>>();
        for range in [
            0..0,
            1..0,
            0..1,
            1..2,
            2..7,
            7..8,
            source_len / 2..source_len / 2 + 1,
            0..source_len,
            source_len..source_len + 10,
            0..usize::MAX,
            usize::MAX..usize::MAX,
        ] {
            let overlaps = |node: &&Node<'_>| {
                !range.is_empty()
                    && node.start_byte() < node.end_byte()
                    && node.start_byte() < range.end
                    && node.end_byte() > range.start
            };
            let expected = preorder
                .iter()
                .filter(overlaps)
                .copied()
                .collect::<Vec<_>>();
            check_pipeline(
                || root.preorder().overlapping_bytes(range.clone()),
                &expected,
            );
            check_pipeline(
                || root.preorder().rev().overlapping_bytes(range.clone()),
                &expected.iter().rev().copied().collect::<Vec<_>>(),
            );
            let expected = postorder
                .iter()
                .filter(overlaps)
                .copied()
                .collect::<Vec<_>>();
            check_pipeline(
                || root.postorder().overlapping_bytes(range.clone()),
                &expected,
            );
            for field in [0, 1, 2, u16::MAX] {
                let expected = preorder
                    .iter()
                    .filter(overlaps)
                    .copied()
                    .filter(|node| kinds.contains(node.kind_id()) && node.field_id() == field)
                    .collect::<Vec<_>>();
                check_pipeline(
                    || {
                        root.preorder()
                            .overlapping_bytes(range.clone())
                            .filter_kind_ids(&kinds)
                            .filter_field_id(field)
                    },
                    &expected,
                );
                let expected = postorder
                    .iter()
                    .filter(overlaps)
                    .copied()
                    .filter(|node| kinds.contains(node.kind_id()) && node.field_id() == field)
                    .collect::<Vec<_>>();
                check_pipeline(
                    || {
                        root.postorder()
                            .overlapping_bytes(range.clone())
                            .filter_kind_ids(&kinds)
                            .filter_field_id(field)
                    },
                    &expected,
                );
            }
        }
        for value in [false, true] {
            check_pipeline(
                || root.preorder().filter_extra(value),
                &preorder
                    .iter()
                    .copied()
                    .filter(|node| node.is_extra() == value)
                    .collect::<Vec<_>>(),
            );
            check_pipeline(
                || root.postorder().filter_missing(value),
                &postorder
                    .iter()
                    .copied()
                    .filter(|node| node.is_missing() == value)
                    .collect::<Vec<_>>(),
            );
        }
        let empty = KindSet::default();
        check_pipeline(|| root.preorder().filter_kind_ids(&empty), &[]);
    }
}

#[test]
fn ranges_filters_waste_and_storage_variants() {
    // Long tokens and whitespace force coordinate-induced group waste.
    let source = format!(
        "{{\"long\": \"{}\", \"values\": [true,\n{}false], \"end\": null}}",
        "x".repeat(700),
        "123,\n".repeat(50)
    );
    let language = json_language();
    for points in [false, true] {
        for symbol_presence in [false, true] {
            let (_, tree) = parse(
                &language,
                &source,
                PackOptions {
                    points,
                    symbol_presence,
                    initial_group_capacity: 1,
                    ..Default::default()
                },
            );
            check_ranges(&tree, source.len());
            let compact = tree.repack().unwrap();
            let grammar = Grammar::new(&language).unwrap();
            let borrowed = Tree::from_bytes_borrowed(&grammar, compact.as_bytes()).unwrap();
            check_ranges(&borrowed, source.len());
        }
    }
}

#[test]
fn empty_missing_and_error_nodes() {
    let language = json_language();
    let mut has_empty = false;
    let mut has_missing = false;
    let mut has_error = false;
    for source in ["", "{\"a\": }", "[1,", "{bad}", "[1 2]", "{\"a\" 1}"] {
        let (_, tree) = parse(&language, source, PackOptions::default());
        check_ranges(&tree, source.len());
        for node in tree.root_node().node_iterator().unwrap() {
            has_empty |= node.start_byte() == node.end_byte();
            has_missing |= node.is_missing();
            has_error |= node.is_error();
        }
        let kinds = KindSet::new([u16::MAX]);
        let expected = tree
            .root_node()
            .node_iterator()
            .unwrap()
            .filter(|node| node.is_error())
            .collect::<Vec<_>>();
        check_pipeline(
            || tree.root_node().preorder().filter_kind_ids(&kinds),
            &expected,
        );
        // Encoded error IDs occupy otherwise invalid public kind IDs.
        for kind in [
            language.node_kind_count() as u16,
            language.node_kind_count() as u16 + 1,
        ] {
            let kinds = KindSet::new([kind]);
            check_pipeline(|| tree.root_node().preorder().filter_kind_ids(&kinds), &[]);
        }
    }
    assert!(has_empty && has_missing && has_error);
}

#[test]
fn dense_id_filters() {
    let source = format!(
        "{{\"items\": [{}null], \"bad\": invalid}}",
        "[1,true],".repeat(40)
    );
    let language = json_language();
    let (_, tree) = parse(&language, &source, PackOptions::default());
    let root = tree.root_node();
    let nodes = root.node_iterator().unwrap().collect::<Vec<_>>();
    let kinds = nodes
        .iter()
        .map(|node| node.kind_id())
        .collect::<HashSet<_>>();
    for kind in kinds.into_iter().chain([u16::MAX - 1, 32768]) {
        let kinds = KindSet::new([kind]);
        let expected = nodes
            .iter()
            .copied()
            .filter(|node| node.kind_id() == kind)
            .collect::<Vec<_>>();
        check_pipeline(|| root.preorder().filter_kind_ids(&kinds), &expected);
    }
    for field in [0, 1, 2, 32768, u16::MAX] {
        let expected = nodes
            .iter()
            .copied()
            .filter(|node| node.field_id() == field)
            .collect::<Vec<_>>();
        check_pipeline(|| root.preorder().filter_field_id(field), &expected);
    }
    let number = language.id_for_node_kind("number", true);
    for ids in [
        vec![number, u16::MAX],
        vec![number, u16::MAX, u16::MAX - 1],
        vec![number, u16::MAX, language.node_kind_count() as u16, 32768],
        vec![number, u16::MAX, u16::MAX - 1, 32768, 32769],
        vec![32768, 32769],
    ] {
        let kinds = KindSet::new(ids);
        let expected = nodes
            .iter()
            .copied()
            .filter(|node| kinds.contains(node.kind_id()))
            .collect::<Vec<_>>();
        check_pipeline(|| root.preorder().filter_kind_ids(&kinds), &expected);
    }
}

fn check_fixed_kinds<const N: usize>(root: Node<'_>, ids: [u16; N]) {
    let expected = root
        .node_iterator()
        .unwrap()
        .filter(|node| ids.contains(&node.kind_id()))
        .collect::<Vec<_>>();
    check_pipeline(|| root.preorder().filter_kind_ids(ids), &expected);
    check_pipeline(|| root.preorder().filter_kind_ids(&ids), &expected);
    assert_eq!(
        root.descendants_matching_kinds(ids).collect::<Vec<_>>(),
        expected
    );
    let dynamic = KindSet::new(ids);
    let postorder = root
        .postorder()
        .filter_kind_ids(&dynamic)
        .nodes()
        .collect::<Vec<_>>();
    check_pipeline(|| root.postorder().filter_kind_ids(ids), &postorder);
    let range = root.start_byte() + 1..root.end_byte().saturating_sub(1);
    let filtered = root
        .preorder()
        .overlapping_bytes(range.clone())
        .filter_kind_ids(&dynamic)
        .filter_field_id(0)
        .nodes()
        .collect::<Vec<_>>();
    check_pipeline(
        || {
            root.preorder()
                .overlapping_bytes(range.clone())
                .filter_kind_ids(ids)
                .filter_field_id(0)
        },
        &filtered,
    );
}

#[test]
fn fixed_kind_sets() {
    let language = json_language();
    let source = format!(
        "{{\"items\": [{}null], \"bad\": invalid}}",
        "[1,true],".repeat(40)
    );
    let (_, tree) = parse(&language, &source, PackOptions::default());
    let root = tree.root_node();
    let number = language.id_for_node_kind("number", true);
    let array = language.id_for_node_kind("array", true);
    let roots = [
        root,
        root.preorder()
            .filter_kind_ids([array])
            .nodes()
            .next()
            .unwrap(),
    ];
    for root in roots {
        check_fixed_kinds(root, []);
        check_fixed_kinds(root, [number]);
        check_fixed_kinds(root, [32768]);
        check_fixed_kinds(root, [u16::MAX]);
        check_fixed_kinds(root, [u16::MAX - 1]);
        check_fixed_kinds(root, [32768, number]);
        check_fixed_kinds(root, [number, array, 32768]);
        check_fixed_kinds(root, [number, array, number, u16::MAX]);
        check_fixed_kinds(
            root,
            [
                number,
                array,
                u16::MAX,
                u16::MAX - 1,
                32768,
                32769,
                number,
                array,
            ],
        );
        check_fixed_kinds(root, [number; 16]);
        check_fixed_kinds(root, [32768; 16]);
    }
    use tree_squatter::traits::NodeLike;
    let (native, tree) = parse(&language, &source, PackOptions::default());
    let kinds = [number, array];
    let native_matches = NodeLike::descendants_matching_kinds(native.root_node(), kinds)
        .map(|node| (node.kind_id(), node.start_byte(), node.end_byte()))
        .collect::<Vec<_>>();
    let packed_matches = NodeLike::descendants_matching_kinds(tree.root_node(), &kinds)
        .map(describe)
        .collect::<Vec<_>>();
    assert_eq!(native_matches, packed_matches);
    assert_eq!(
        NodeLike::descendants_matching_kinds(native.root_node(), []).count(),
        0
    );
}

fn check_field_set<const N: usize>(root: Node<'_>, fields: [u16; N]) {
    let expected = root
        .node_iterator()
        .unwrap()
        .filter(|node| fields.contains(&node.field_id()))
        .collect::<Vec<_>>();
    check_pipeline(|| root.preorder().filter_field_ids(fields), &expected);
    check_pipeline(|| root.preorder().filter_field_ids(&fields), &expected);
    let dynamic = IdSet::new(fields);
    check_pipeline(|| root.preorder().filter_field_ids(&dynamic), &expected);
    let postorder = root
        .postorder()
        .nodes()
        .filter(|node| fields.contains(&node.field_id()))
        .collect::<Vec<_>>();
    check_pipeline(|| root.postorder().filter_field_ids(fields), &postorder);
    check_pipeline(|| root.postorder().filter_field_ids(&dynamic), &postorder);
    let kinds = [
        root.kind_id(),
        expected.first().map_or(0, |node| node.kind_id()),
    ];
    let range = root.start_byte()..root.end_byte();
    let combined = expected
        .iter()
        .copied()
        .filter(|node| kinds.contains(&node.kind_id()) && node.start_byte() < node.end_byte())
        .collect::<Vec<_>>();
    check_pipeline(
        || {
            root.preorder()
                .overlapping_bytes(range.clone())
                .filter_kind_ids(kinds)
                .filter_field_ids(fields)
        },
        &combined,
    );
}

#[test]
fn field_sets() {
    let language = json_language();
    let source = format!(
        "{{\"items\": [{}null], \"other\": 42}}",
        "{\"value\": [1,true]},".repeat(40)
    );
    let (_, tree) = parse(&language, &source, PackOptions::default());
    let root = tree.root_node();
    let key = language.field_id_for_name("key").unwrap().get();
    let value = language.field_id_for_name("value").unwrap().get();
    let subtree = root
        .preorder()
        .filter_kind_ids([language.id_for_node_kind("array", true)])
        .nodes()
        .next()
        .unwrap();
    for root in [root, subtree] {
        check_field_set(root, []);
        check_field_set(root, [0]);
        check_field_set(root, [key]);
        check_field_set(root, [key, value]);
        check_field_set(root, [0, key, value]);
        check_field_set(root, [key, value, key, 32768]);
        check_field_set(root, [0, key, value, 32768, u16::MAX]);
        check_field_set(root, [u16::MAX; 8]);
    }
}

#[test]
fn supertype_membership() {
    let languages = [
        (json_language(), "{\"a\": [1, true, null]}"),
        (
            unsafe {
                tree_sitter::Language::from_raw(tree_sitter_c_sharp::LANGUAGE.into_raw()().cast())
            },
            "class Example { int field = 1; int Method(int value) { return value + field; } }",
        ),
    ];
    let mut exercised_dictionary = false;
    for (language, source) in languages {
        let (_, tree) = parse(&language, source, PackOptions::default());
        let nodes = tree
            .root_node()
            .node_iterator()
            .unwrap()
            .collect::<Vec<_>>();
        let supertypes = (0..language.node_kind_count() as u16)
            .filter(|&id| language.node_kind_is_supertype(id))
            .collect::<Vec<_>>();
        exercised_dictionary |= supertypes.len() > 8;
        let mut matches = 0;
        for supertype in supertypes.into_iter().chain([u16::MAX]) {
            let expected = nodes
                .iter()
                .copied()
                .filter(|node| node.has_supertype(supertype))
                .collect::<Vec<_>>();
            matches += expected.len();
            check_pipeline(
                || tree.root_node().preorder().filter_supertype_id(supertype),
                &expected,
            );
        }
        assert!(matches > 0);
    }
    assert!(exercised_dictionary);
}

#[test]
fn composition_and_reverse_preserve_membership() {
    let language = unsafe {
        tree_sitter::Language::from_raw(tree_sitter_c_sharp::LANGUAGE.into_raw()().cast())
    };
    let source = "// comment\nclass Example { int field = 1; int Method(int value) { return value + field; } }";
    let (_, tree) = parse(&language, source, PackOptions::default());
    check_ranges(&tree, source.len());
    let root = tree.root_node();
    let preorder = root.node_iterator().unwrap().collect::<Vec<_>>();
    let kinds = KindSet::new(preorder.iter().step_by(2).map(|node| node.kind_id()));
    let other_kinds = KindSet::new(preorder.iter().step_by(3).map(|node| node.kind_id()));
    let expected = root
        .postorder()
        .nodes()
        .filter(|node| {
            kinds.contains(node.kind_id())
                && other_kinds.contains(node.kind_id())
                && !node.is_extra()
        })
        .collect::<Vec<_>>();
    check_pipeline(
        || {
            root.postorder()
                .filter_kind_ids(&kinds)
                .filter_kind_ids(&other_kinds)
                .filter_extra(false)
        },
        &expected,
    );
    check_pipeline(
        || {
            root.postorder()
                .rev()
                .filter_extra(false)
                .filter_kind_ids(&other_kinds)
                .filter_kind_ids(&kinds)
        },
        &expected.iter().rev().copied().collect::<Vec<_>>(),
    );
    assert!(preorder.iter().any(|node| node.is_extra()));
}
