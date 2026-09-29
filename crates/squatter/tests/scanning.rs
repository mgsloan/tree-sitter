mod support;

use std::collections::HashSet;
use tree_sitter::Point;
use tree_squatter::{
    FieldId, FieldSet, Forest, GrammarId, KindId, KindSet, Language, Node, PackOptions,
    scan::{GroupScan, Scan},
};

use support::{
    NodeDescription, c_sharp_language, check_consumption, describe_node, json_language,
    native_orders, pack_native,
};

fn describe(node: Node<'_>) -> NodeDescription {
    describe_node(node.kind_id(), node.byte_range())
}

// TreeCursor navigation is independent of the group scans.
fn reference_preorder(root: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = root.walk();
    let mut nodes = Vec::new();
    loop {
        nodes.push(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return nodes;
            }
        }
    }
}

fn check_pipeline<'tree, S: GroupScan<'tree>>(
    make: impl Fn() -> Scan<'tree, S>,
    expected: &[Node<'tree>],
) {
    assert_eq!(make().nodes().collect::<Vec<_>>(), expected);
    assert_eq!(make().count(), expected.len());
    assert_eq!(make().rev().count(), expected.len());
    assert_eq!(make().nodes().count(), expected.len());
    let reversed = expected.iter().rev().copied().collect::<Vec<_>>();
    assert_eq!(make().rev().nodes().collect::<Vec<_>>(), reversed);
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
            .rev()
            .groups()
            .flat_map(|group| group.nodes())
            .collect::<Vec<_>>(),
        reversed
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
            assert_eq!(
                group.node(node.slot().raw() - group.first_slot().raw()),
                Some(node)
            );
        }
        assert_eq!(group.node(u32::MAX), None);
    }
    assert_eq!(count, expected.len());
    check_consumption(|| make().nodes(), expected);
    check_consumption(|| make().rev().nodes(), &reversed);
}

fn check_native_order<'tree, S: GroupScan<'tree>>(
    make: impl Fn() -> Scan<'tree, S>,
    expected: &[NodeDescription],
) {
    assert_eq!(make().nodes().map(describe).collect::<Vec<_>>(), expected);
    assert_eq!(
        make().rev().nodes().map(describe).collect::<Vec<_>>(),
        expected.iter().rev().copied().collect::<Vec<_>>()
    );
}

#[test]
fn orders_subtrees_groups_and_directions() {
    let source = format!(
        "{{\"a\": [1, {{\"b\": true}}, null], \"wide\": [{}0]}}",
        "[1,2],".repeat(90)
    );
    let (native, tree) = pack_native(
        &json_language(),
        &source,
        PackOptions {
            initial_group_capacity: 1,
            ..Default::default()
        },
    );
    fn require_send_sync(_: impl Send + Sync) {}
    let root = tree.root_node();
    let kinds = KindSet::new([root.kind_id()]);
    require_send_sync(root.preorder());
    require_send_sync(root.preorder().rev().nodes());
    require_send_sync(root.postorder().nodes());
    require_send_sync(root.postorder().rev().filter_kind_ids(&kinds).groups());
    require_send_sync(
        root.preorder()
            .overlapping_points(Point::new(0, 0)..Point::new(1, 0))
            .nodes(),
    );
    require_send_sync(root.postorder().ending_at_point(Point::new(0, 7)).groups());
    require_send_sync(root.preorder().within_bytes(0..7));
    let group = root.all().groups().next().unwrap();
    require_send_sync(group);
    require_send_sync(group.group());
    require_send_sync(group.nodes());

    let (preorder, postorder) = native_orders(native.root_node());
    check_native_order(|| tree.root_node().preorder(), &preorder);
    check_native_order(|| tree.root_node().postorder(), &postorder);
    for root in reference_preorder(tree.root_node()) {
        let expected = reference_preorder(root);
        check_pipeline(|| root.preorder(), &expected);
        check_pipeline(|| root.all(), &expected);
        let mut cursor = root.walk();
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
fn check_ranges(tree: &Forest, source_len: usize) {
    let all = reference_preorder(tree.root_node());
    let kinds = KindSet::new(all.iter().step_by(3).map(|node| node.kind_id()));
    let roots = all.iter().copied().step_by((all.len() / 15).max(1));
    for root in roots {
        let preorder = reference_preorder(root);
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
                    && node.start_byte() < range.end
                    && (node.end_byte() > range.start || node.start_byte() >= range.start)
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
            for field in [0, 1, 2, u16::MAX].map(FieldId::from_raw) {
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
            let (_, tree) = pack_native(
                &language,
                &source,
                PackOptions {
                    points,
                    symbol_presence: &|_| symbol_presence,
                    initial_group_capacity: 1,
                    ..Default::default()
                },
            );
            check_ranges(&tree, source.len());
            let compact = tree.repack().unwrap();
            let grammar = Language::new(&language).unwrap();
            let borrowed =
                Forest::from_bytes_borrowed(std::slice::from_ref(&grammar), compact.as_bytes())
                    .unwrap();
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
        let (_, mut tree) = pack_native(&language, source, PackOptions::default());
        check_ranges(&tree, source.len());
        for node in reference_preorder(tree.root_node()) {
            has_empty |= node.start_byte() == node.end_byte();
            has_missing |= node.is_missing();
            has_error |= node.is_error();
        }
        for indexed in [true, false] {
            if !indexed {
                tree.drop_presence_cache();
            }
            let kinds = KindSet::new([KindId::ERROR]);
            let expected = reference_preorder(tree.root_node())
                .into_iter()
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
                32768,
                u16::MAX - 2,
            ]
            .map(KindId::from_raw)
            {
                let kinds = KindSet::new([kind]);
                check_pipeline(|| tree.root_node().preorder().filter_kind_ids(&kinds), &[]);
            }
        }
    }
    assert!(has_empty && has_missing && has_error);
}

// Compare terminal operations and directions against ordinary node attributes.
macro_rules! check_selection {
    ($root:expr, $preorder:expr, $postorder:expr, $method:ident, $argument:expr, $matches:expr) => {{
        let argument = $argument;
        let matches = $matches;
        let expected = $preorder
            .iter()
            .copied()
            .filter(matches)
            .collect::<Vec<_>>();
        check_pipeline(|| $root.preorder().$method(argument.clone()), &expected);

        assert_eq!(
            $root
                .all()
                .$method(argument.clone())
                .filter_kind_ids([$root.kind_id()])
                .filter_field_id(None)
                .nodes()
                .collect::<Vec<_>>(),
            expected
                .into_iter()
                .filter(|node| node.kind_id() == $root.kind_id() && node.field_id().is_none())
                .collect::<Vec<_>>()
        );
        let expected = $postorder
            .iter()
            .copied()
            .filter(matches)
            .collect::<Vec<_>>();
        check_pipeline(|| $root.postorder().$method(argument.clone()), &expected);
    }};
}

#[test]
fn range_seeks_across_subtrees() {
    let source = format!(
        "[{}[{}0{}],\n{}[1,2]]",
        format!("[{}0],\n", "[1,2],".repeat(100)).repeat(8),
        "[".repeat(80),
        "]".repeat(80),
        " ".repeat(700),
    );
    for points in [false, true] {
        let (_, tree) = pack_native(
            &json_language(),
            &source,
            PackOptions {
                points,
                initial_group_capacity: 1,
                ..Default::default()
            },
        );
        let all = reference_preorder(tree.root_node());
        for root in all.iter().copied().step_by(all.len() / 5) {
            let samples: Vec<_> = all.iter().step_by(all.len() / 7).collect();
            check_range_selections(
                root,
                samples.iter().map(|node| node.byte_range()).collect(),
                samples
                    .iter()
                    .map(|node| node.start_position()..node.end_position())
                    .collect(),
            );
        }
    }
}

fn check_position_selections(root: Node<'_>) {
    let preorder = reference_preorder(root);
    let samples = preorder.iter().step_by((preorder.len() / 4).max(1)).chain(
        preorder
            .iter()
            .filter(|node| node.start_byte() == node.end_byte()),
    );
    let mut byte_ranges = vec![0..0, 0..1, 0..usize::MAX, usize::MAX..usize::MAX];
    let mut point_ranges = vec![
        Point::new(0, 0)..Point::new(0, 0),
        Point::new(0, 0)..Point::new(0, 1),
        Point::new(0, 0)..Point::new(usize::MAX, usize::MAX),
        Point::new(0, usize::MAX)..Point::new(1, 0),
        Point::new(1, 0)..Point::new(0, usize::MAX),
        Point::new(usize::MAX, 0)..Point::new(usize::MAX, usize::MAX),
    ];
    for node in samples {
        let start = node.start_byte();
        let end = node.end_byte();
        byte_ranges.extend([
            start..end,
            end..start,
            start..start,
            end..end,
            start..start + 1,
            end..end + 1,
            start.saturating_sub(1)..start,
        ]);
        let start = node.start_position();
        let end = node.end_position();
        point_ranges.extend([
            start..end,
            end..start,
            start..start,
            end..end,
            start..Point::new(start.row, start.column + 1),
            end..Point::new(end.row, end.column + 1),
            Point::new(start.row, start.column.saturating_sub(1))..start,
        ]);
    }
    check_range_selections(root, byte_ranges, point_ranges);
}

fn check_range_selections(
    root: Node<'_>,
    byte_ranges: Vec<std::ops::Range<usize>>,
    point_ranges: Vec<std::ops::Range<Point>>,
) {
    let preorder = reference_preorder(root);
    let postorder = root.postorder().nodes().collect::<Vec<_>>();
    for range in byte_ranges {
        check_selection!(
            root,
            preorder,
            postorder,
            overlapping_bytes,
            range.clone(),
            |node: &Node<'_>| {
                let start = node.start_byte();
                let end = node.end_byte();
                !range.is_empty()
                    && (if start == end {
                        range.contains(&start)
                    } else {
                        start < range.end && range.start < end
                    })
            }
        );
        check_selection!(
            root,
            preorder,
            postorder,
            within_bytes,
            range.clone(),
            |node: &Node<'_>| {
                let start = node.start_byte();
                let end = node.end_byte();
                range.start <= range.end && range.start <= start && end <= range.end
            }
        );
        check_selection!(
            root,
            preorder,
            postorder,
            containing_bytes,
            range.clone(),
            |node: &Node<'_>| {
                let start = node.start_byte();
                let end = node.end_byte();
                range.start <= range.end && start <= range.start && range.end <= end
            }
        );
        check_selection!(
            root,
            preorder,
            postorder,
            starting_in_bytes,
            range.clone(),
            |node: &Node<'_>| {
                let start = node.start_byte();
                !range.is_empty() && (range.contains(&start))
            }
        );
        check_selection!(
            root,
            preorder,
            postorder,
            ending_in_bytes,
            range.clone(),
            |node: &Node<'_>| {
                let end = node.end_byte();
                !range.is_empty() && (range.contains(&end))
            }
        );
        let position = range.start;
        check_selection!(
            root,
            preorder,
            postorder,
            containing_byte,
            position,
            |node: &Node<'_>| { (node.start_byte()..node.end_byte()).contains(&position) }
        );
        check_selection!(
            root,
            preorder,
            postorder,
            starting_at_byte,
            position,
            |node: &Node<'_>| { node.start_byte() == position }
        );
        check_selection!(
            root,
            preorder,
            postorder,
            ending_at_byte,
            position,
            |node: &Node<'_>| { node.end_byte() == position }
        );
    }
    for range in point_ranges {
        check_selection!(
            root,
            preorder,
            postorder,
            overlapping_points,
            range.clone(),
            |node: &Node<'_>| {
                let start = node.start_position();
                let end = node.end_position();
                !range.is_empty()
                    && (if start == end {
                        range.contains(&start)
                    } else {
                        start < range.end && range.start < end
                    })
            }
        );
        check_selection!(
            root,
            preorder,
            postorder,
            within_points,
            range.clone(),
            |node: &Node<'_>| {
                let start = node.start_position();
                let end = node.end_position();
                range.start <= range.end && range.start <= start && end <= range.end
            }
        );
        check_selection!(
            root,
            preorder,
            postorder,
            containing_points,
            range.clone(),
            |node: &Node<'_>| {
                let start = node.start_position();
                let end = node.end_position();
                range.start <= range.end && start <= range.start && range.end <= end
            }
        );
        check_selection!(
            root,
            preorder,
            postorder,
            starting_in_points,
            range.clone(),
            |node: &Node<'_>| {
                let start = node.start_position();
                !range.is_empty() && (range.contains(&start))
            }
        );
        check_selection!(
            root,
            preorder,
            postorder,
            ending_in_points,
            range.clone(),
            |node: &Node<'_>| {
                let end = node.end_position();
                !range.is_empty() && (range.contains(&end))
            }
        );
        let position = range.start;
        check_selection!(
            root,
            preorder,
            postorder,
            containing_point,
            position,
            |node: &Node<'_>| { (node.start_position()..node.end_position()).contains(&position) }
        );
        check_selection!(
            root,
            preorder,
            postorder,
            starting_at_point,
            position,
            |node: &Node<'_>| { node.start_position() == position }
        );
        check_selection!(
            root,
            preorder,
            postorder,
            ending_at_point,
            position,
            |node: &Node<'_>| { node.end_position() == position }
        );
    }
}

#[test]
fn range_and_position_relations() {
    let language = json_language();
    let sources = [
        String::new(),
        "[1".into(),
        "{\"a\": [1,\n2, }".into(),
        format!(
            "[{}{}[1,2]]",
            " \n".repeat(300),
            "[\"é🦀\",123],\n".repeat(20)
        ),
        format!(
            "{{\"long\": \"{}\",\n\"values\": [{}0]}}",
            "x".repeat(700),
            "[1,\n2],".repeat(20)
        ),
    ];
    for source in sources {
        for points in [false, true] {
            let (_, tree) = pack_native(
                &language,
                &source,
                PackOptions {
                    points,
                    initial_group_capacity: 1,
                    ..Default::default()
                },
            );
            let compact = tree.repack().unwrap();
            let grammar = Language::new(&language).unwrap();
            let borrowed =
                Forest::from_bytes_borrowed(std::slice::from_ref(&grammar), compact.as_bytes())
                    .unwrap();
            for tree in [&tree, &borrowed] {
                let roots = reference_preorder(tree.root_node());
                for root in [roots[0], roots[roots.len() / 2], roots[roots.len() - 1]]
                    .into_iter()
                    .chain(roots.iter().copied().filter(|node| node.is_missing()))
                {
                    check_position_selections(root);
                    if root.is_missing() {
                        let byte = root.start_byte();
                        let point = root.start_position();
                        assert_eq!(root.all().containing_byte(byte).count(), 0);
                        assert_eq!(root.all().containing_bytes(byte..byte).count(), 1);
                        assert_eq!(root.all().containing_point(point).count(), 0);
                        assert_eq!(root.all().containing_points(point..point).count(), 1);
                    }
                }
            }
        }
    }
}

#[test]
fn id_set_intersection() {
    let cases: &[(&[u16], &[u16], &[u16])] = &[
        (&[], &[], &[]),
        (&[], &[1], &[]),
        (&[1, 2], &[3], &[]),
        (&[0, 63, 64, 65], &[0, 63, 64, 65], &[0, 63, 64, 65]),
        (
            &[u16::MAX, 64, 63, 64, 0],
            &[64, 65, u16::MAX],
            &[64, u16::MAX],
        ),
    ];
    for &(first, second, expected) in cases {
        let first = KindSet::new(first.iter().copied().map(KindId::from_raw));
        let second = KindSet::new(second.iter().copied().map(KindId::from_raw));
        for intersection in [first.intersection(&second), second.intersection(&first)] {
            assert_eq!(intersection.is_empty(), expected.is_empty());
            let actual = (0..=u16::MAX)
                .filter(|&id| intersection.contains(KindId::from_raw(id)))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
        }
    }
}

fn id_filter_source() -> String {
    format!(
        "{{\"items\": [{}null], \"bad\": invalid}}",
        "[1,true],".repeat(40)
    )
}

#[test]
fn dense_id_filters() {
    let source = id_filter_source();
    let language = json_language();
    let (_, tree) = pack_native(&language, &source, PackOptions::default());
    let root = tree.root_node();
    let grammar = Language::new(&language).unwrap();
    let nodes = reference_preorder(root);
    let kinds = nodes
        .iter()
        .map(|node| node.kind_id())
        .collect::<HashSet<_>>();
    for kind in kinds
        .into_iter()
        .chain([KindId::from_raw(u16::MAX - 1), KindId::from_raw(32768)])
    {
        let kinds = KindSet::new([kind]);
        let expected = nodes
            .iter()
            .copied()
            .filter(|node| node.kind_id() == kind)
            .collect::<Vec<_>>();
        check_pipeline(|| root.preorder().filter_kind_ids(&kinds), &expected);
    }
    for field in [0, 1, 2, 32768, u16::MAX].map(FieldId::from_raw) {
        let expected = nodes
            .iter()
            .copied()
            .filter(|node| node.field_id() == field)
            .collect::<Vec<_>>();
        check_pipeline(|| root.preorder().filter_field_id(field), &expected);
    }
    let number = grammar.kind_id_for_name("number", true).unwrap();
    for ids in [
        vec![number, KindId::ERROR],
        vec![number, KindId::ERROR, KindId::from_raw(u16::MAX - 1)],
        vec![
            number,
            KindId::ERROR,
            KindId::from_raw(language.node_kind_count() as u16),
            KindId::from_raw(32768),
        ],
        vec![
            number,
            KindId::ERROR,
            KindId::from_raw(u16::MAX - 1),
            KindId::from_raw(32768),
            KindId::from_raw(32769),
        ],
        vec![KindId::from_raw(32768), KindId::from_raw(32769)],
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

#[test]
fn sparse_cursor_pipelines() {
    let language = json_language();
    let grammar = Language::new(&language).unwrap();
    let source = format!(
        "[{}0]",
        format!("[{}true,false,null],", "1,".repeat(1400)).repeat(12)
    );
    let [truth, falsity, null] =
        ["true", "false", "null"].map(|kind| grammar.kind_id_for_name(kind, true).unwrap());
    let ids = [truth, falsity, null, truth];
    let kinds = KindSet::new(ids);
    let other = KindSet::new([truth, null]);
    for symbol_presence in [false, true] {
        let (_, tree) = pack_native(
            &language,
            &source,
            PackOptions {
                symbol_presence: &|_| symbol_presence,
                ..Default::default()
            },
        );
        assert!(tree.group_count().div_ceil(32) >= 12);
        {
            let root = tree.root_node();
            let booleans = [truth, falsity];
            let kinds = KindSet::new(
                (0..language.node_kind_count() as u16)
                    .chain([32768, u16::MAX])
                    .map(KindId::from_raw),
            );
            let nodes = reference_preorder(root);
            let expected = nodes
                .iter()
                .copied()
                .filter(|node| booleans.contains(&node.kind_id()))
                .collect::<Vec<_>>();
            check_pipeline(
                || {
                    root.preorder()
                        .filter_kind_ids(booleans)
                        .filter_kind_ids(&kinds)
                },
                &expected,
            );
            for start in [0, 7, source.len() / 2, source.len() - 16] {
                let range = start..start + 16;
                let expected = nodes
                    .iter()
                    .copied()
                    .filter(|node| {
                        range.start <= node.start_byte()
                            && node.end_byte() <= range.end
                            && kinds.contains(node.kind_id())
                    })
                    .collect::<Vec<_>>();
                check_pipeline(
                    || {
                        root.preorder()
                            .within_bytes(range.clone())
                            .filter_kind_ids(&kinds)
                    },
                    &expected,
                );
            }
        }
        let nodes = reference_preorder(tree.root_node());
        let subtree = nodes
            .iter()
            .copied()
            .find(|node| node.kind() == "array" && node.start_byte() > source.len() / 3)
            .unwrap();
        for root in [tree.root_node(), subtree] {
            let nodes = reference_preorder(root);
            let expected = nodes
                .iter()
                .copied()
                .filter(|node| kinds.contains(node.kind_id()))
                .collect::<Vec<_>>();
            check_pipeline(|| root.all().filter_kind_ids(ids), &expected);
            check_pipeline(|| root.all().filter_kind_ids(&kinds), &expected);
            for range in [
                0..source.len(),
                source.len() / 4..source.len() * 3 / 4,
                source.len() / 2..source.len() / 2 + 7,
            ] {
                let expected = nodes
                    .iter()
                    .copied()
                    .filter(|node| {
                        node.start_byte() < range.end
                            && (node.end_byte() > range.start || node.start_byte() >= range.start)
                            && kinds.contains(node.kind_id())
                            && other.contains(node.kind_id())
                    })
                    .collect::<Vec<_>>();
                check_pipeline(
                    || {
                        root.all()
                            .overlapping_bytes(range.clone())
                            .filter_kind_ids(ids)
                            .filter_extra(false)
                            .filter_kind_ids(&other)
                    },
                    &expected,
                );
                check_pipeline(
                    || {
                        root.all()
                            .overlapping_bytes(range.clone())
                            .filter_kind_ids(&other)
                            .filter_extra(false)
                            .filter_kind_ids(&kinds)
                    },
                    &expected,
                );
            }
        }
    }
}

#[test]
fn prepared_kind_sets() {
    let language = json_language();
    let source = format!(
        "{{\"items\": [{}null], \"bad\": invalid}}",
        "[1,true,false,\"text\"],".repeat(25)
    );
    for symbol_presence in [false, true] {
        let (_, tree) = pack_native(
            &language,
            &source,
            PackOptions {
                symbol_presence: &|_| symbol_presence,
                ..Default::default()
            },
        );
        let root = tree.root_node();
        let nodes = reference_preorder(root);
        let other = KindSet::new(nodes.iter().step_by(3).map(|node| node.kind_id()));
        for length in [0, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 23] {
            for start in [0, 32768, u16::MAX - 23] {
                let kinds = KindSet::new(
                    (start..start + length)
                        .chain([u16::MAX, u16::MAX - 1])
                        .map(KindId::from_raw),
                );
                let plain = KindSet::new((start..start + length).map(KindId::from_raw));
                for kinds in [&plain, &kinds] {
                    let expected = nodes
                        .iter()
                        .copied()
                        .filter(|node| kinds.contains(node.kind_id()))
                        .collect::<Vec<_>>();
                    check_pipeline(|| root.all().filter_kind_ids(kinds), &expected);
                    let range = 10..source.len() - 10;
                    let expected = expected
                        .into_iter()
                        .filter(|node| {
                            range.start <= node.start_byte()
                                && node.end_byte() <= range.end
                                && node.field_id().is_none()
                                && !node.is_extra()
                                && other.contains(node.kind_id())
                        })
                        .collect::<Vec<_>>();
                    check_pipeline(
                        || {
                            root.all()
                                .within_bytes(range.clone())
                                .filter_field_id(None)
                                .filter_extra(false)
                                .filter_kind_ids(kinds)
                                .filter_kind_ids(&other)
                        },
                        &expected,
                    );
                    check_pipeline(
                        || {
                            root.all()
                                .within_bytes(range.clone())
                                .filter_kind_ids(&other)
                                .filter_kind_ids(kinds)
                                .filter_extra(false)
                                .filter_field_id(None)
                        },
                        &expected,
                    );
                }
            }
        }
    }
}

#[test]
fn indexed_kind_filters() {
    let language = json_language();
    let grammar = Language::new(&language).unwrap();
    let source = format!(
        "[true,[{}false],[{}true],[{}false]]",
        "1,".repeat(1800),
        "[\"text\",2],".repeat(300),
        "3,".repeat(5400),
    );
    let [truth, falsity, string, number, absent] = ["true", "false", "string", "number", "null"]
        .map(|name| grammar.kind_id_for_name(name, true).unwrap());
    for symbol_presence in [false, true] {
        let (_, packed) = pack_native(
            &language,
            &source,
            PackOptions {
                symbol_presence: &|_| symbol_presence,
                ..Default::default()
            },
        );
        assert!(packed.group_count() > 32);
        let borrowed =
            Forest::from_bytes_borrowed(std::slice::from_ref(&grammar), packed.as_bytes()).unwrap();
        for tree in [&packed, &borrowed] {
            let nodes = reference_preorder(tree.root_node());
            let subtree = nodes
                .iter()
                .copied()
                .find(|node| node.start_byte() == 6 && node.kind() == "array")
                .unwrap();
            for root in [tree.root_node(), subtree] {
                for ids in [
                    [truth, falsity, absent, KindId::from_raw(32768)],
                    [string, truth, absent, KindId::ERROR],
                    [
                        number,
                        number,
                        KindId::from_raw(u16::MAX - 1),
                        KindId::from_raw(32768),
                    ],
                    [absent, absent, KindId::from_raw(32768), KindId::ERROR],
                ] {
                    check_array_kinds(root, ids);
                    let kinds = KindSet::new(ids);
                    let all = reference_preorder(root);
                    let expected = all
                        .iter()
                        .copied()
                        .filter(|node| kinds.contains(node.kind_id()))
                        .collect::<Vec<_>>();
                    check_pipeline(|| root.all().filter_kind_ids(&kinds), &expected);
                    for range in [
                        0..source.len(),
                        5..41,
                        source.len() / 3..source.len() * 2 / 3,
                        source.len() - 20..source.len(),
                    ] {
                        let expected = all
                            .iter()
                            .copied()
                            .filter(|node| {
                                kinds.contains(node.kind_id())
                                    && range.start <= node.start_byte()
                                    && node.end_byte() <= range.end
                            })
                            .collect::<Vec<_>>();
                        check_pipeline(
                            || {
                                root.all()
                                    .within_bytes(range.clone())
                                    .filter_kind_ids(&kinds)
                            },
                            &expected,
                        );
                        check_pipeline(
                            || {
                                root.all()
                                    .within_points(
                                        Point::new(0, range.start)..Point::new(0, range.end),
                                    )
                                    .filter_kind_ids(ids)
                            },
                            &expected,
                        );
                        let overlapping = all
                            .iter()
                            .copied()
                            .filter(|node| {
                                kinds.contains(node.kind_id())
                                    && node.start_byte() < range.end
                                    && (node.end_byte() > range.start
                                        || node.start_byte() >= range.start)
                            })
                            .collect::<Vec<_>>();
                        check_pipeline(
                            || {
                                root.all()
                                    .overlapping_bytes(range.clone())
                                    .filter_kind_ids(ids)
                            },
                            &overlapping,
                        );
                    }
                    let expected = expected
                        .into_iter()
                        .filter(|node| [truth, string].contains(&node.kind_id()))
                        .collect::<Vec<_>>();
                    check_pipeline(
                        || {
                            root.all()
                                .filter_kind_ids(ids)
                                .filter_kind_ids([truth, string])
                        },
                        &expected,
                    );
                }
            }
        }
    }
}

fn check_array_kinds<const N: usize>(root: Node<'_>, ids: [KindId; N]) {
    let expected = reference_preorder(root)
        .into_iter()
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
        .filter_field_id(None)
        .nodes()
        .collect::<Vec<_>>();
    check_pipeline(
        || {
            root.preorder()
                .overlapping_bytes(range.clone())
                .filter_kind_ids(ids)
                .filter_field_id(None)
        },
        &filtered,
    );
}

#[test]
fn fixed_kind_sets() {
    let language = json_language();
    let source = id_filter_source();
    let (native, tree) = pack_native(&language, &source, PackOptions::default());
    let root = tree.root_node();
    let grammar = Language::new(&language).unwrap();
    let number = grammar.kind_id_for_name("number", true).unwrap();
    let array = grammar.kind_id_for_name("array", true).unwrap();
    let roots = [
        root,
        root.preorder()
            .filter_kind_ids([array])
            .nodes()
            .next()
            .unwrap(),
    ];
    for root in roots {
        check_array_kinds(root, []);
        check_array_kinds(root, [number]);
        check_array_kinds(root, [KindId::from_raw(32768)]);
        check_array_kinds(root, [KindId::ERROR]);
        check_array_kinds(root, [KindId::from_raw(u16::MAX - 1)]);
        check_array_kinds(root, [KindId::from_raw(32768), number]);
        check_array_kinds(root, [number, array, KindId::from_raw(32768)]);
        check_array_kinds(root, [number, array, number, KindId::ERROR]);
        check_array_kinds(
            root,
            [
                number,
                array,
                KindId::ERROR,
                KindId::from_raw(u16::MAX - 1),
                KindId::from_raw(32768),
                KindId::from_raw(32769),
                number,
                array,
            ],
        );
        check_array_kinds(root, [number; 16]);
        check_array_kinds(root, [KindId::from_raw(32768); 16]);
    }
    use tree_squatter::traits::NodeLike;
    let kinds = [number, array];
    let native_matches = NodeLike::descendants_matching_kinds(native.root_node(), kinds)
        .map(|node| describe_node(node.kind_id(), node.byte_range()))
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

fn check_field_set<const N: usize>(root: Node<'_>, fields: [Option<FieldId>; N]) {
    let expected = reference_preorder(root)
        .into_iter()
        .filter(|node| fields.contains(&node.field_id()))
        .collect::<Vec<_>>();
    check_pipeline(|| root.preorder().filter_field_ids(fields), &expected);
    check_pipeline(|| root.preorder().filter_field_ids(&fields), &expected);
    let dynamic = FieldSet::new(fields);
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
        expected
            .first()
            .map_or(KindId::from_raw(0), |node| node.kind_id()),
    ];
    let range = root.start_byte()..root.end_byte();
    let combined = expected
        .iter()
        .copied()
        .filter(|node| {
            kinds.contains(&node.kind_id())
                && !range.is_empty()
                && node.start_byte() < range.end
                && (node.end_byte() > range.start || node.start_byte() >= range.start)
        })
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
    let (_, tree) = pack_native(&language, &source, PackOptions::default());
    let root = tree.root_node();
    let grammar = Language::new(&language).unwrap();
    let key = Some(grammar.field_id_for_name("key").unwrap());
    for node in root.all().filter_field_id(key) {
        assert_eq!(
            node.kind_id(),
            grammar.kind_id_for_name("string", true).unwrap()
        );
        assert_eq!(tree.node_at_slot(node.slot()), Some(node));
    }
    let value = Some(grammar.field_id_for_name("value").unwrap());
    let subtree = root
        .preorder()
        .filter_kind_ids([grammar.kind_id_for_name("array", true).unwrap()])
        .nodes()
        .next()
        .unwrap();
    for root in [root, subtree] {
        check_field_set(root, []);
        check_field_set(root, [None]);
        check_field_set(root, [key]);
        check_field_set(root, [None, key]);
        check_field_set(root, [key, value]);
        check_field_set(root, [None, key, value]);
        check_field_set(root, [key, value, key, FieldId::from_raw(32768)]);
        check_field_set(
            root,
            [
                None,
                key,
                value,
                FieldId::from_raw(32768),
                FieldId::from_raw(u16::MAX),
            ],
        );
        check_field_set(root, [FieldId::from_raw(u16::MAX); 8]);
    }
}

#[test]
fn supertype_membership() {
    let json_source = format!("{{\"a\": [{}null]}}", "[1,true,null],".repeat(40));
    let languages = [
        (json_language(), json_source.as_str()),
        (
            c_sharp_language(),
            "// comment\nclass Example { int field = 1; int Method(int value) { return value + field; } }",
        ),
    ];
    let mut exercised_direct = false;
    let mut exercised_dictionary = false;
    for (language, source) in languages {
        let (_, tree) = pack_native(&language, source, PackOptions::default());
        let nodes = reference_preorder(tree.root_node());
        let postorder = tree.root_node().postorder().nodes().collect::<Vec<_>>();
        let supertypes = (0..language.node_kind_count() as u16)
            .filter(|&id| language.node_kind_is_supertype(id))
            .collect::<Vec<_>>();
        exercised_direct |= !supertypes.is_empty() && supertypes.len() <= 8;
        exercised_dictionary |= supertypes.len() > 8;
        let mut matches = 0;
        for supertype in supertypes
            .into_iter()
            .chain([u16::MAX])
            .map(GrammarId::from_raw)
        {
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
            let expected = postorder
                .iter()
                .copied()
                .filter(|node| node.has_supertype(supertype))
                .collect::<Vec<_>>();
            check_pipeline(
                || tree.root_node().postorder().filter_supertype_id(supertype),
                &expected,
            );
        }
        assert!(matches > 0);
        if language == c_sharp_language() {
            check_ranges(&tree, source.len());
            let root = tree.root_node();
            let preorder = reference_preorder(root);
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
    }
    assert!(exercised_direct && exercised_dictionary);
}

#[test]
fn deep_and_wide_postorder() {
    for source in [
        format!("{}0{}", "[".repeat(512), "]".repeat(512)),
        format!("[{}0]", "[0,1],".repeat(2048)),
    ] {
        let (native, tree) = pack_native(&json_language(), &source, PackOptions::default());
        let (_, expected) = native_orders(native.root_node());
        let root = tree.root_node();
        check_native_order(|| root.postorder(), &expected);
        let nodes = root.postorder().nodes().collect::<Vec<_>>();
        check_consumption(|| root.postorder().nodes(), &nodes);
        check_consumption(
            || root.postorder().rev().nodes(),
            &nodes.into_iter().rev().collect::<Vec<_>>(),
        );
    }
}
