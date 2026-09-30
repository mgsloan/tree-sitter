mod support;

use support::{c_language, json_language};

use tree_squatter::{Forest, Language, PackOptions};

macro_rules! compare_attributes {
    ($actual:expr, $expected:expr; $($method:ident),* $(,)?) => {
        $(assert_eq!($actual.$method(), $expected.$method(), "{} at {:?}", stringify!($method), $actual);)*
    };
}

#[test]
fn navigation_and_indexed_ranges_survive_loading() {
    let language = c_language();
    let grammar = Language::new(&language).unwrap();
    let source = "// π\ntypedef int T; T value;\nint f(int x) { /* extra */ return x + ; }\nint g() { return 1 }";
    let native = support::parse_native(&language, source);
    for points in [false, true] {
        let actual = Forest::pack_with_options(
            &grammar,
            &native,
            PackOptions {
                points,
                ..Default::default()
            },
        )
        .unwrap();
        let mut expected =
            Forest::from_bytes(std::slice::from_ref(&grammar), actual.as_bytes()).unwrap();
        assert_eq!(actual.as_bytes(), expected.as_bytes());
        if let Some(points) = actual.point_data() {
            let points =
                tree_squatter::PointsData::copy_from_bytes(&expected, points.as_bytes()).unwrap();
            expected.set_point_data(points).unwrap();
        }
        let expected_nodes: std::collections::HashMap<_, _> = expected
            .root_node()
            .preorder()
            .nodes()
            .map(|node| (node.id(), node))
            .collect();
        let mut actual_cursor = actual.root_node().walk();
        let mut expected_cursor = expected.root_node().walk();

        for node in actual.root_node().preorder() {
            let reference = expected_nodes[&node.id()];
            assert_eq!(node.kind_id(), reference.kind_id());
            assert_eq!(node.grammar_id(), reference.grammar_id());
            assert_eq!(node.field_id(), reference.field_id());
            compare_attributes!(node, reference;
                kind, grammar_name, field_name,
                byte_range, start_position, end_position, is_named, is_extra,
                is_missing, is_error, has_error, descendant_count,
                child_count, named_child_count);
            macro_rules! compare_navigation {
                ($($method:ident),*) => { $(assert_eq!(node.$method().map(|node| node.id()), reference.$method().map(|node| node.id()), "{} at {node:?}", stringify!($method));)* };
            }
            compare_navigation!(
                parent,
                prev_sibling,
                next_sibling,
                prev_named_sibling,
                next_named_sibling,
                prev_preorder,
                next_preorder
            );
            assert_eq!(
                node.children(&mut actual_cursor)
                    .map(|node| node.id())
                    .collect::<Vec<_>>(),
                reference
                    .children(&mut expected_cursor)
                    .map(|node| node.id())
                    .collect::<Vec<_>>()
            );

            actual_cursor.reset(node);
            expected_cursor.reset(reference);
            assert_eq!(
                actual_cursor.goto_last_child(),
                expected_cursor.goto_last_child()
            );
            loop {
                assert_eq!(actual_cursor.node().id(), expected_cursor.node().id());
                let moved = actual_cursor.goto_previous_sibling();
                assert_eq!(moved, expected_cursor.goto_previous_sibling());
                if !moved {
                    break;
                }
            }
            assert_eq!(actual_cursor.goto_parent(), expected_cursor.goto_parent());
            assert_eq!(actual_cursor.node().id(), expected_cursor.node().id());
        }

        for root in actual
            .root_node()
            .preorder()
            .nodes()
            .step_by((actual.root_node().descendant_count() / 10).max(1))
        {
            let reference = expected_nodes[&root.id()];
            use tree_sitter::Point;
            for (start, end) in [
                (
                    Point::new(u32::MAX as usize, 0),
                    Point::new(u32::MAX as usize, u32::MAX as usize),
                ),
                (Point::new(1, 0), Point::new(0, u32::MAX as usize)),
                (Point::new(0, u32::MAX as usize), Point::new(1, 0)),
                (Point::new(0, 500), Point::new(0, 501)),
            ] {
                assert_eq!(
                    root.descendant_for_point_range(start, end)
                        .map(|node| node.id()),
                    reference
                        .descendant_for_point_range(start, end)
                        .map(|node| node.id()),
                );
                assert_eq!(
                    root.named_descendant_for_point_range(start, end)
                        .map(|node| node.id()),
                    reference
                        .named_descendant_for_point_range(start, end)
                        .map(|node| node.id()),
                );
            }
            for start in (0..=source.len() + 1).step_by((source.len() / 40).max(1)) {
                for length in [0, 1, 5, 1000] {
                    let end = start + length;
                    assert_eq!(
                        root.descendant_for_byte_range(start, end)
                            .map(|node| node.id()),
                        reference
                            .descendant_for_byte_range(start, end)
                            .map(|node| node.id()),
                        "{root:?} {start}..{end}"
                    );
                    assert_eq!(
                        root.named_descendant_for_byte_range(start, end)
                            .map(|node| node.id()),
                        reference
                            .named_descendant_for_byte_range(start, end)
                            .map(|node| node.id())
                    );

                    let position = |byte: usize| {
                        if !points {
                            return tree_sitter::Point::new(0, byte);
                        }
                        let prefix = &source.as_bytes()[..byte.min(source.len())];
                        let row = prefix.iter().filter(|byte| **byte == b'\n').count();
                        let column = byte
                            - prefix
                                .iter()
                                .rposition(|byte| *byte == b'\n')
                                .map_or(0, |index| index + 1);
                        tree_sitter::Point::new(row, column)
                    };
                    let (start, end) = (position(start), position(end));
                    assert_eq!(
                        root.descendant_for_point_range(start, end)
                            .map(|node| node.id()),
                        reference
                            .descendant_for_point_range(start, end)
                            .map(|node| node.id()),
                        "{root:?} {start:?}..{end:?}"
                    );
                    assert_eq!(
                        root.named_descendant_for_point_range(start, end)
                            .map(|node| node.id()),
                        reference
                            .named_descendant_for_point_range(start, end)
                            .map(|node| node.id())
                    );
                }
            }
        }
    }
}

#[test]
fn indexed_points_follow_attachment_across_wide_trees() {
    use tree_sitter::Point;
    use tree_squatter::PointsData;

    let language = json_language();
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let source = format!("[{}0]\n", "\"é\",\r\n".repeat(20_000));
    let native = parser.parse(&source, None).unwrap();
    let line_starts: Vec<_> = std::iter::once(0)
        .chain(
            source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        )
        .collect();
    let point = |byte| {
        let row = line_starts.partition_point(|&start| start <= byte) - 1;
        Point::new(row, byte - line_starts[row])
    };
    let mut tree = Forest::pack_with_options(
        &grammar,
        &native,
        PackOptions {
            points: true,
            ..PackOptions::default()
        },
    )
    .unwrap();
    let address = tree.as_bytes().as_ptr();
    let points = tree.point_data().unwrap().as_bytes().to_vec();

    for stored in [false, true, false] {
        if stored {
            tree.set_point_data(PointsData::copy_from_bytes(&tree, &points).unwrap())
                .unwrap();
        } else {
            tree.drop_point_data();
        }
        for (root, reference) in [
            (tree.root_node(), native.root_node()),
            (
                tree.root_node()
                    .named_child(tree_squatter::NamedChildIx::new(0))
                    .unwrap(),
                native.root_node().named_child(0).unwrap(),
            ),
        ] {
            for start in (0..=source.len()).step_by(997).chain([source.len()]) {
                for end in [start, (start + 1).min(source.len()), source.len()] {
                    let point = |byte| {
                        if stored {
                            point(byte)
                        } else {
                            Point::new(0, byte)
                        }
                    };
                    for named in [false, true] {
                        let actual = if named {
                            root.named_descendant_for_point_range(point(start), point(end))
                        } else {
                            root.descendant_for_point_range(point(start), point(end))
                        };
                        let expected = if named {
                            reference.named_descendant_for_byte_range(start, end)
                        } else {
                            reference.descendant_for_byte_range(start, end)
                        };
                        assert_eq!(
                            actual.map(|node| (node.byte_range(), node.kind_id().raw())),
                            expected.map(|node| (node.byte_range(), node.kind_id())),
                            "stored={stored} named={named} {start}..{end}",
                        );
                    }
                }
            }
        }
        assert_eq!(tree.as_bytes().as_ptr(), address);
    }
}

#[test]
fn child_iterators_preserve_cursor_state() {
    use tree_squatter::{
        ChildIx, FieldId, NamedChildIx,
        traits::{CursorLike, NodeLike},
    };

    fn check<'tree, A: NodeLike<'tree>, B: NodeLike<'tree>>(actual: A, expected: B) {
        let mut actual_cursor = actual.walk();
        let mut expected_cursor = expected.walk();
        for limit in 0..=actual.child_count().ix() + 1 {
            for mode in 0..4 {
                let field = FieldId::from_raw(1).unwrap();
                let actual_nodes: Vec<_> = match mode {
                    0 => actual.children(&mut actual_cursor).take(limit).collect(),
                    1 => actual
                        .named_children(&mut actual_cursor)
                        .take(limit)
                        .collect(),
                    2 => actual
                        .children_by_field_id(field, &mut actual_cursor)
                        .take(limit)
                        .collect(),
                    _ => actual
                        .children_by_field_name("body", &mut actual_cursor)
                        .take(limit)
                        .collect(),
                };
                let expected_nodes: Vec<_> = match mode {
                    0 => expected
                        .children(&mut expected_cursor)
                        .take(limit)
                        .collect(),
                    1 => expected
                        .named_children(&mut expected_cursor)
                        .take(limit)
                        .collect(),
                    2 => expected
                        .children_by_field_id(field, &mut expected_cursor)
                        .take(limit)
                        .collect(),
                    _ => expected
                        .children_by_field_name("body", &mut expected_cursor)
                        .take(limit)
                        .collect(),
                };
                assert_eq!(
                    actual_nodes
                        .iter()
                        .map(|node| (node.kind(), node.byte_range()))
                        .collect::<Vec<_>>(),
                    expected_nodes
                        .iter()
                        .map(|node| (node.kind(), node.byte_range()))
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    actual_cursor.node().byte_range(),
                    expected_cursor.node().byte_range()
                );
                assert_eq!(actual_cursor.node().kind(), expected_cursor.node().kind());
                assert_eq!(actual_cursor.depth(), expected_cursor.depth());
                assert_eq!(actual_cursor.field_id(), expected_cursor.field_id());
                assert_eq!(actual_cursor.field_name(), expected_cursor.field_name());
            }
        }
        let before = actual_cursor.node();
        assert!(
            actual
                .children_by_field_name("unknown-field", &mut actual_cursor)
                .next()
                .is_none()
        );
        assert!(actual_cursor.node() == before);

        for index in 0..=actual.child_count().raw() {
            assert_eq!(
                actual.field_name_for_child(ChildIx::new(index)),
                expected.field_name_for_child(ChildIx::new(index))
            );
        }
        for index in 0..=actual.named_child_count().raw() {
            assert_eq!(
                actual.field_name_for_named_child(NamedChildIx::new(index)),
                expected.field_name_for_named_child(NamedChildIx::new(index))
            );
        }

        actual_cursor.reset(actual);
        assert_eq!(actual_cursor.field_id(), None);
        assert_eq!(actual_cursor.field_name(), None);
        let mut cloned = actual_cursor.clone();
        if cloned.goto_first_child() {
            assert!(actual_cursor.node() == actual);
            actual_cursor.reset_to(&cloned);
            assert!(actual_cursor.node() == cloned.node());
            assert_eq!(actual_cursor.depth(), cloned.depth());
            assert_eq!(actual_cursor.field_id(), cloned.field_id());
            assert!(actual_cursor.goto_parent());
            assert_eq!(cloned.depth(), 1);
        }
    }

    let language = c_language();
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    for source in [
        "",
        "int x;",
        "int f() { return (1 + ); }",
        "typedef int T; T x;",
        "int f(){ /*comment*/ }",
    ] {
        let native = parser.parse(source, None).unwrap();
        let tree = Forest::pack(&grammar, &native).unwrap();
        for (actual, expected) in tree
            .root_node()
            .preorder()
            .nodes()
            .zip(NodeLike::preorder(native.root_node()))
        {
            check(actual, expected);
        }
    }
}
