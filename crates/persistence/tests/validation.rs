use tree_sitter_squatter::{PackOptions, Query, QueryCursor, Tree};

fn language() -> tree_sitter::Language {
    unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) }
}

fn pack(language: &tree_sitter::Language, source: &str, presence: bool) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(language).unwrap();
    Tree::pack_with_options(
        &parser.parse(source, None).unwrap(),
        PackOptions {
            repack: true,
            symbol_presence: presence,
            ..PackOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn safety_loader_does_not_verify_presence_membership() {
    let language = language();
    let source = format!("[{}0]", "1,".repeat(4096));
    let original = pack(&language, &source, true);
    let without = pack(&language, &source, false);
    assert!(original.group_count() > 32);
    assert_eq!(original.group_count(), without.group_count());
    // JSON has no supertype dictionary: the presence section is the whole tail.
    assert_eq!(&original.as_bytes()[12..16], &[0; 4]);
    assert_eq!(&without.as_bytes()[12..16], &[0; 4]);
    assert!(original.as_bytes().len() > without.as_bytes().len());
    for byte in [0, 0xff, 0x55] {
        let mut bytes = original.as_bytes().to_vec();
        bytes[without.as_bytes().len()..].fill(byte);
        assert!(Tree::from_bytes(&language, &bytes).is_err());
        let loaded = Tree::from_bytes_safety_checked(&language, &bytes).unwrap();
        assert_eq!(
            loaded.root_node().preorder().count(),
            original.root_node().preorder().count()
        );
        for group in 0..loaded.group_count() {
            for symbol in 0..language.node_kind_count() as u16 {
                let _ = loaded.group_has_symbol(group, symbol);
            }
        }
        let query = Query::new(&language, "(number) @n").unwrap();
        let mut cursor = QueryCursor::new();
        let mut execution = cursor.execute(&query, loaded.root_node(), source.as_bytes());
        // Membership may be wrong, but execution must stay safe and terminate.
        while execution.next_match().is_some() {}
    }
}

#[test]
fn safety_loader_rejects_truncated_sections_and_invalid_headers() {
    let language = language();
    let original = pack(&language, "{\"key\": [true, 42]}", true);
    for length in 0..original.as_bytes().len() {
        assert!(
            Tree::from_bytes_safety_checked(&language, &original.as_bytes()[..length]).is_err()
        );
    }
    for (offset, value) in [(0, 0), (4, 0), (8, u32::MAX), (12, 257)] {
        let mut bytes = original.as_bytes().to_vec();
        bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
        assert!(Tree::from_bytes_safety_checked(&language, &bytes).is_err());
    }
}

#[test]
fn mutated_slabs_are_rejected_or_support_bounded_traversal() {
    let language = language();
    let original = pack(&language, "{\"key\": [true, 42]}", false);
    let mut state = 42u32;
    for trial in 0..512 {
        let mut bytes = original.as_bytes().to_vec();
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let index = state as usize % bytes.len();
        bytes[index] ^= 1 << (trial % 8);
        let Ok(tree) = Tree::from_bytes_safety_checked(&language, &bytes) else {
            continue;
        };
        for (count, node) in tree.root_node().preorder().enumerate() {
            assert!(count < tree.slot_count() as usize);
            let _ = (node.kind(), node.grammar_name(), node.field_name());
            let _ = (node.parent(), node.next_sibling(), node.prev_sibling());
            let _ = (
                node.child_count(),
                node.named_child_count(),
                node.descendant_count(),
            );
            assert!(node.start_byte() <= node.end_byte());
        }
    }
}
