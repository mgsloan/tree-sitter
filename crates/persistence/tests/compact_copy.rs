use std::mem::MaybeUninit;
use tree_sitter::{Language, Point};
use tree_squatter::{PackOptions, Query, QueryCursor, Tree};

fn tree_sitter_language() -> Language {
    unsafe { Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) }
}

#[test]
fn compact_copy_matches_repack_for_padded_and_compact_trees() {
    let tree_sitter_language = tree_sitter_language();
    let language = tree_squatter::Language::new(&tree_sitter_language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_language).unwrap();
    for source in [
        "".to_owned(),
        "[1,{\"x\":null}]".to_owned(),
        format!("[{}]", vec!["{\"x\":[1,2,3]}"; 200].join(",")),
    ] {
        let native = parser.parse(&source, None).unwrap();
        for presence in [false, true] {
            for (repack, points) in [(false, false), (false, true), (true, false), (true, true)] {
                let tree = Tree::pack_with_options(
                    &language,
                    &native,
                    PackOptions {
                        initial_group_capacity: 1024,
                        repack,
                        symbol_presence: presence,
                        points,
                        ..Default::default()
                    },
                )
                .unwrap();
                let original = tree.as_bytes().to_vec();
                let expected = tree.repack().unwrap();
                assert_eq!(tree.compact_size(), expected.as_bytes().len());
                // Odd offsets exercise unaligned storage and guards catch writes
                // outside the exact reservation, including its alignment padding.
                for offset in 0..8 {
                    let mut storage =
                        vec![MaybeUninit::new(0xA5); tree.compact_size() + offset + 8];
                    let actual = tree
                        .copy_compact_into(&mut storage[offset..offset + tree.compact_size()])
                        .unwrap();
                    assert_eq!(actual, expected.as_bytes());
                    Tree::from_bytes(&language, actual).unwrap();
                    for byte in storage[..offset]
                        .iter()
                        .chain(&storage[offset + tree.compact_size()..])
                    {
                        assert_eq!(unsafe { byte.assume_init() }, 0xA5);
                    }
                }
                let mut short = vec![MaybeUninit::new(0xA5); tree.compact_size() - 1];
                assert!(tree.copy_compact_into(&mut short).is_err());
                assert!(
                    short
                        .iter()
                        .all(|byte| unsafe { byte.assume_init() } == 0xA5)
                );
                let mut uninitialized = vec![MaybeUninit::uninit(); tree.compact_size()];
                assert_eq!(
                    tree.copy_compact_into(&mut uninitialized).unwrap(),
                    expected.as_bytes()
                );
                assert_eq!(tree.as_bytes(), original);
            }
        }
    }
}

#[test]
fn point_free_trees_use_byte_offsets_as_single_line_points() {
    let tree_sitter_language = tree_sitter_language();
    let language = tree_squatter::Language::new(&tree_sitter_language).unwrap();
    let source = b"[\n  1,\n  2\n]";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_language).unwrap();
    let native = parser.parse(source, None).unwrap();
    let tree = Tree::pack_with_options(
        &language,
        &native,
        PackOptions {
            points: false,
            ..PackOptions::default()
        },
    )
    .unwrap();
    assert!(!tree.has_points());
    for node in tree.root_node().preorder() {
        assert_eq!(node.start_position(), Point::new(0, node.start_byte()));
        assert_eq!(node.end_position(), Point::new(0, node.end_byte()));
    }
    assert_eq!(
        tree.root_node().descendant_for_byte_range(3, 4),
        tree.root_node()
            .descendant_for_point_range(Point::new(0, 3), Point::new(0, 4))
    );

    let query = Query::new(&language, "(_) @node").unwrap();
    let captures = |cursor: &mut QueryCursor| {
        let mut execution = cursor.execute(&query, tree.root_node(), source.as_slice());
        let mut ranges = Vec::new();
        while let Some((result, index)) = execution.next_capture() {
            ranges.push(result.captures()[index.0 as usize].node.byte_range());
        }
        ranges
    };
    let mut byte_cursor = QueryCursor::new();
    byte_cursor.set_byte_range(3..4);
    let mut point_cursor = QueryCursor::new();
    point_cursor.set_point_range(Point::new(0, 3)..Point::new(0, 4));
    assert_eq!(captures(&mut byte_cursor), captures(&mut point_cursor));

    let compact = tree.repack().unwrap();
    let loaded = Tree::from_bytes(&language, compact.as_bytes()).unwrap();
    assert!(!loaded.has_points());
    assert_eq!(
        loaded.root_node().end_position(),
        Point::new(0, source.len())
    );
}
