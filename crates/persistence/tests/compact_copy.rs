use std::mem::MaybeUninit;
use tree_sitter_squatter::{PackOptions, Tree};

#[test]
fn compact_copy_matches_repack_for_padded_and_compact_trees() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    for source in [
        "".to_owned(),
        "[1,{\"x\":null}]".to_owned(),
        format!("[{}]", vec!["{\"x\":[1,2,3]}"; 200].join(",")),
    ] {
        let native = parser.parse(&source, None).unwrap();
        for presence in [false, true] {
            for repack in [false, true] {
                let tree = Tree::pack_with_options(
                    &native,
                    PackOptions {
                        initial_group_capacity: 1024,
                        repack,
                        symbol_presence: presence,
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
