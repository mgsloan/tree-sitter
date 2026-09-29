mod support;

use support::{c_sharp_language, json_language};

use tree_squatter::{Language, Tree};

#[test]
fn language_cache_round_trips_and_outlives_tree_sitter_language() {
    let language = c_sharp_language();
    let candidate = Language::new(&language).unwrap();
    let bytes = candidate.cache().unwrap();
    let restored = Language::from_cache(&language, &bytes).unwrap();
    let clone = restored.clone();
    drop(restored);
    drop(candidate);
    drop(language);
    assert_eq!(clone.cache().unwrap(), bytes);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn shared_coordinates_narrow_like_tree_sitter() {
    let language = json_language();
    let grammar = Language::new(&language).unwrap();
    let native = support::parse_native(&language, "[1,\n2]");
    let tree = Tree::pack(&grammar, &native).unwrap();
    let root = tree.root_node();
    let expected = native.root_node();
    let wrap = 1usize << 32;
    for start in [0, 1, 4, 6, wrap, wrap + 1, wrap + 4, usize::MAX] {
        for end in [start, start.saturating_add(1)] {
            assert_eq!(
                root.descendant_for_byte_range(start, end)
                    .map(|node| node.byte_range()),
                expected
                    .descendant_for_byte_range(start, end)
                    .map(|node| node.byte_range())
            );
            let point = tree_sitter::Point::new(start, end);
            assert_eq!(
                root.descendant_for_point_range(point, point)
                    .map(|node| node.byte_range()),
                expected
                    .descendant_for_point_range(point, point)
                    .map(|node| node.byte_range())
            );
        }
        assert_eq!(
            root.first_child_for_byte(start)
                .map(|node| node.byte_range()),
            expected
                .first_child_for_byte(start)
                .map(|node| node.byte_range())
        );
        let mut cursor = root.walk();
        let mut native_cursor = expected.walk();
        assert_eq!(
            cursor
                .goto_first_child_for_byte(start)
                .map(|index| index.get_raw() as usize),
            native_cursor.goto_first_child_for_byte(start)
        );
        let point = tree_sitter::Point::new(wrap, start);
        cursor.reset(root);
        native_cursor.reset(expected);
        assert_eq!(
            cursor
                .goto_first_child_for_point(point)
                .map(|index| index.get_raw() as usize),
            native_cursor.goto_first_child_for_point(point)
        );
    }
    let array = root
        .named_child(tree_squatter::NamedChildIx::new(0))
        .unwrap();
    assert_eq!(array.child_by_field_name([255]), None);
}
