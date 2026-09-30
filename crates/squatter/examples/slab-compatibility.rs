use std::{env, fs, mem::MaybeUninit};
use tree_squatter::{Forest, Language, PackOptions, PointsData, PresenceCache};

fn compare(expected: &Forest, actual: &Forest) {
    assert_eq!(expected.has_points(), actual.has_points());
    let expected_root = expected.root_node();
    let actual_root = actual.root_node();
    assert_eq!(
        expected_root.descendant_count(),
        actual_root.descendant_count()
    );
    for (left, right) in expected_root.preorder().nodes().zip(actual_root.preorder()) {
        assert_eq!(left.id(), right.id());
        assert_eq!(left.attributes(), right.attributes());
        assert_eq!(left.field_id(), right.field_id());
        assert_eq!(left.child_count(), right.child_count());
        assert_eq!(left.named_child_count(), right.named_child_count());
        assert_eq!(left.descendant_count(), right.descendant_count());
        assert_eq!(
            left.parent().map(|node| node.id()),
            right.parent().map(|node| node.id())
        );
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = env::args().collect();
    assert!(
        arguments.len() == 3 || arguments.len() == 4,
        "usage: slab-compatibility little|big OUTPUT_PREFIX [REFERENCE_PREFIX]"
    );
    assert_eq!(cfg!(target_endian = "big"), arguments[1] == "big");
    let tree_sitter_language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let language = Language::new(&tree_sitter_language)?;
    let source = format!(
        "[{}[{}0{}],\"{}\",{{\"bad\":}}]",
        "{\"key\": [1,true,null]},\n".repeat(100),
        "[".repeat(80),
        "]".repeat(80),
        "x".repeat(70000)
    );
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_language)?;
    let parsed = parser.parse(&source, None).unwrap();
    for variant in 0..8 {
        let tree = Forest::pack_with_options(
            &language,
            &parsed,
            PackOptions {
                compact: variant & 1 != 0,
                points: variant & 2 == 0,
                symbol_presence: &|_| variant & 4 == 0,
                ..Default::default()
            },
        )?;
        fs::write(format!("{}-{variant}.slab", arguments[2]), tree.as_bytes())?;
        if let Some(points) = tree.point_data() {
            fs::write(
                format!("{}-{variant}.points", arguments[2]),
                points.as_bytes(),
            )?;
        }
        if let Some(presence) = tree.presence_cache() {
            fs::write(
                format!("{}-{variant}.presence", arguments[2]),
                presence.as_bytes(),
            )?;
        }
        if let Some(prefix) = arguments.get(3) {
            let bytes = fs::read(format!("{prefix}-{variant}.slab"))?;
            assert!(
                bytes == tree.as_bytes(),
                "different bytes for variant {variant}"
            );
            let mut copied = Forest::from_bytes(std::slice::from_ref(&language), &bytes)?;
            let borrowed =
                Forest::from_bytes_borrowed(std::slice::from_ref(&language), copied.as_bytes())?;
            let mut checked =
                Forest::from_bytes_safety_checked(std::slice::from_ref(&language), &bytes)?;
            assert!(!copied.has_points());
            assert!(copied.presence_cache().is_none());
            compare(&copied, &borrowed);
            compare(&copied, &checked);
            drop(borrowed);
            if let Some(points) = tree.point_data() {
                let bytes = fs::read(format!("{prefix}-{variant}.points"))?;
                assert_eq!(bytes, points.as_bytes());
                copied.set_point_data(PointsData::copy_from_bytes(&copied, &bytes)?)?;
                checked.set_point_data(PointsData::copy_from_bytes(&checked, &bytes)?)?;
            }
            if let Some(presence) = tree.presence_cache() {
                let bytes = fs::read(format!("{prefix}-{variant}.presence"))?;
                assert_eq!(bytes, presence.as_bytes());
                copied.set_presence_cache(PresenceCache::copy_from_bytes(&copied, &bytes)?)?;
                checked.set_presence_cache(PresenceCache::copy_from_bytes(&checked, &bytes)?)?;
            }
            compare(&tree, &copied);
            compare(&tree, &checked);
            let mut compact = vec![MaybeUninit::uninit(); tree.compact_size()];
            assert_eq!(
                tree.copy_compact_into(&mut compact)?,
                tree.to_compacted()?.as_bytes()
            );
        }
    }
    Ok(())
}
