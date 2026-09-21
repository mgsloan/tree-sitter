use std::{env, fs, mem::MaybeUninit};
use tree_squatter::{Grammar, PackOptions, PointData, PresenceCache, SlotIx, Tree};

fn compare(expected: &Tree, actual: &Tree) {
    assert_eq!(expected.slot_count(), actual.slot_count());
    assert_eq!(expected.has_points(), actual.has_points());
    for slot in 0..expected.slot_count() {
        let left = expected.node_at_slot(SlotIx::new(slot));
        let right = actual.node_at_slot(SlotIx::new(slot));
        assert_eq!(left.is_some(), right.is_some());
        if let (Some(left), Some(right)) = (left, right) {
            assert_eq!(left.attributes(), right.attributes());
            assert_eq!(left.field_id(), right.field_id());
            assert_eq!(left.child_count(), right.child_count());
            assert_eq!(left.named_child_count(), right.named_child_count());
            assert_eq!(left.descendant_count(), right.descendant_count());
            assert_eq!(
                left.parent().map(|node| node.slot()),
                right.parent().map(|node| node.slot())
            );
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = env::args().collect();
    assert!(
        arguments.len() == 3 || arguments.len() == 4,
        "usage: slab-compatibility little|big OUTPUT_PREFIX [REFERENCE_PREFIX]"
    );
    assert_eq!(cfg!(target_endian = "big"), arguments[1] == "big");
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Grammar::new(&language)?;
    let source = format!(
        "[{}[{}0{}],\"{}\",{{\"bad\":}}]",
        "{\"key\": [1,true,null]},\n".repeat(100),
        "[".repeat(80),
        "]".repeat(80),
        "x".repeat(70000)
    );
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language)?;
    let parsed = parser.parse(&source, None).unwrap();
    for variant in 0..16 {
        let tree = Tree::pack_with_options(
            &grammar,
            &parsed,
            PackOptions {
                initial_group_capacity: variant & 1,
                repack: variant & 2 != 0,
                points: variant & 4 == 0,
                symbol_presence: variant & 8 == 0,
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
            let mut copied = Tree::from_bytes(&grammar, &bytes)?;
            let borrowed = Tree::from_bytes_borrowed(&grammar, copied.as_bytes())?;
            let mut checked = Tree::from_bytes_safety_checked(&grammar, &bytes)?;
            assert!(!copied.has_points());
            assert!(copied.presence_cache().is_none());
            compare(&copied, &borrowed);
            compare(&copied, &checked);
            drop(borrowed);
            if let Some(points) = tree.point_data() {
                let bytes = fs::read(format!("{prefix}-{variant}.points"))?;
                assert_eq!(bytes, points.as_bytes());
                copied.set_point_data(PointData::copy_from_bytes(&copied, &bytes)?)?;
                checked.set_point_data(PointData::copy_from_bytes(&checked, &bytes)?)?;
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
                tree.repack()?.as_bytes()
            );
        }
    }
    Ok(())
}
