use tree_squatter_rust::{Grammar, PackOptions, Tree};

macro_rules! compare_attributes {
    ($actual:expr, $expected:expr; $($method:ident),* $(,)?) => {
        $(assert_eq!($actual.$method(), $expected.$method(), "{} at {:?}", stringify!($method), $actual);)*
    };
}

#[test]
fn navigation_and_indexed_ranges_match_reference() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) };
    let grammar = Grammar::new(&language).unwrap();
    let reference_grammar = tree_squatter::Grammar::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let wide = "int value;\n".repeat(1000);
    let deep = format!(
        "int f() {{ return {}1{}; }}",
        "(".repeat(200),
        ")".repeat(200)
    );

    for source in [
        "",
        "// π\n",
        "int x = ;",
        "int f() { return 1 }",
        "int f(int x) { /* extra */ return x + 1; }",
        &wide,
        &deep,
    ] {
        let native = parser.parse(source, None).unwrap();
        for points in [false, true] {
            let actual = Tree::pack_with_options(
                &grammar,
                &native,
                PackOptions {
                    points,
                    ..Default::default()
                },
            )
            .unwrap();
            let expected = tree_squatter::Tree::pack_with_options(
                &reference_grammar,
                &native,
                tree_squatter::PackOptions {
                    points,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut actual_cursor = actual.root_node().walk().unwrap();
            let mut expected_cursor = expected.root_node().walk().unwrap();

            for node in actual.root_node().preorder() {
                let reference = expected.node_at_slot(node.slot()).unwrap();
                compare_attributes!(node, reference;
                    kind_id, kind, grammar_id, grammar_name, field_id, field_name,
                    byte_range, start_position, end_position, is_named, is_extra,
                    is_missing, is_error, has_error, has_changes, descendant_count,
                    child_count, named_child_count);
                macro_rules! compare_navigation {
                    ($($method:ident),*) => { $(assert_eq!(node.$method().map(|node| node.slot()), reference.$method().map(|node| node.slot()), "{} at {node:?}", stringify!($method));)* };
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
                    node.children().map(|node| node.slot()).collect::<Vec<_>>(),
                    reference
                        .children()
                        .map(|node| node.slot())
                        .collect::<Vec<_>>()
                );

                actual_cursor.reset(node);
                expected_cursor.reset(reference);
                assert_eq!(
                    actual_cursor.goto_last_child(),
                    expected_cursor.goto_last_child()
                );
                loop {
                    assert_eq!(actual_cursor.node().slot(), expected_cursor.node().slot());
                    let moved = actual_cursor.goto_previous_sibling();
                    assert_eq!(moved, expected_cursor.goto_previous_sibling());
                    if !moved {
                        break;
                    }
                }
                assert_eq!(actual_cursor.goto_parent(), expected_cursor.goto_parent());
                assert_eq!(actual_cursor.node().slot(), expected_cursor.node().slot());
            }

            for root in actual
                .root_node()
                .preorder()
                .nodes()
                .step_by((actual.root_node().descendant_count() / 10).max(1))
            {
                let reference = expected.node_at_slot(root.slot()).unwrap();
                for start in (0..=source.len() + 1).step_by((source.len() / 40).max(1)) {
                    for length in [0, 1, 5, 1000] {
                        let end = start + length;
                        assert_eq!(
                            root.descendant_for_byte_range(start, end)
                                .map(|node| node.slot()),
                            reference
                                .descendant_for_byte_range(start, end)
                                .map(|node| node.slot()),
                            "{root:?} {start}..{end}"
                        );
                        assert_eq!(
                            root.named_descendant_for_byte_range(start, end)
                                .map(|node| node.slot()),
                            reference
                                .named_descendant_for_byte_range(start, end)
                                .map(|node| node.slot())
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
                                .map(|node| node.slot()),
                            reference
                                .descendant_for_point_range(start, end)
                                .map(|node| node.slot()),
                            "{root:?} {start:?}..{end:?}"
                        );
                        assert_eq!(
                            root.named_descendant_for_point_range(start, end)
                                .map(|node| node.slot()),
                            reference
                                .named_descendant_for_point_range(start, end)
                                .map(|node| node.slot())
                        );
                    }
                }
            }
        }
    }
}
