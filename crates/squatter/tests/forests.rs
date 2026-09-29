mod support;

use std::collections::HashSet;
use tree_squatter::{
    Forest, Language, Node, PackOptions, PackRegion, Packer, PointsData, PresenceCache, StableSlab,
    TreeIx,
};

fn describe(root: Node<'_>) -> Vec<(u16, tree_sitter::Range)> {
    root.preorder()
        .nodes()
        .map(|node| (node.kind_id().raw(), node.range()))
        .collect()
}

struct SlabOwner(Box<[u64]>);
unsafe impl StableSlab for SlabOwner {
    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.0.as_ptr().cast(), self.0.len() * 8) }
    }
}
fn retain(bytes: &[u8]) -> SlabOwner {
    SlabOwner(
        bytes
            .chunks_exact(8)
            .map(|word| u64::from_ne_bytes(word.try_into().unwrap()))
            .collect(),
    )
}

#[test]
fn forest_packing_and_round_trip() {
    let json = support::json_language();
    let c = support::c_language();
    let languages = [
        Language::new(&json).unwrap(),
        Language::new(&c).unwrap(),
        Language::new(&json).unwrap(),
    ];
    let first = support::parse_native(&json, "[1,\n2]");
    let second = support::parse_native(&c, "int answer() { return 42; }");
    let third = support::parse_native(&json, "{\"name\": [3, 4]}");
    let fourth = support::parse_native(&json, "true");
    let positioned = second.root_node_with_offset(200, tree_sitter::Point::new(7, 12));
    let detached = third
        .root_node()
        .named_child(0)
        .unwrap()
        .named_child(0)
        .unwrap()
        .named_child(1)
        .unwrap();
    let roots = [first.root_node(), positioned, detached, fourth.root_node()];
    let mut visited = Vec::new();
    let visits = std::cell::RefCell::new(&mut visited);
    let select = |region: tree_squatter::ForestRegion<'_>| {
        visits
            .borrow_mut()
            .push((region.index().raw(), region.group_count()));
        region.index().raw() != 1
    };
    let (mut forest, mapping) = Packer::new()
        .unwrap()
        .pack_forest(
            vec![
                PackRegion {
                    language: languages[0].clone(),
                    roots: vec![roots[0]],
                },
                PackRegion {
                    language: languages[1].clone(),
                    roots: vec![roots[1]],
                },
                PackRegion {
                    language: languages[2].clone(),
                    roots: vec![roots[2], roots[3]],
                },
            ],
            PackOptions {
                symbol_presence: &select,
                repack: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        mapping.iter().map(|index| index.raw()).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(
        forest
            .regions()
            .map(|region| region.trees().count())
            .collect::<Vec<_>>(),
        [1, 1, 2]
    );
    assert_eq!(visited.len(), 3);
    let mut identities = HashSet::new();
    for (tree, native) in forest.trees().zip(roots) {
        let root = tree.root_node();
        assert!(root.parent().is_none());
        assert!(root.next_sibling().is_none());
        assert!(root.prev_sibling().is_none());
        assert_eq!(root.field_name(), None);
        assert_eq!(root.range(), native.range());
        assert_eq!(root.kind_id().raw(), native.kind_id());
        assert_eq!(root.grammar_id().raw(), native.grammar_id());
        let expected = support::native_orders(native).0;
        let actual: Vec<_> = root
            .preorder()
            .nodes()
            .map(|node| {
                assert!(identities.insert(node.id()));
                assert_eq!(node.id().tree(), root.id().tree());
                if let Some(parent) = node.parent() {
                    assert_eq!(parent.id().tree(), root.id().tree());
                }
                support::describe_node(node.kind_id(), node.byte_range())
            })
            .collect();
        assert_eq!(actual, expected);
        let last = root.preorder().nodes().last().unwrap();
        assert!(last.next_preorder().is_none());
        assert!(root.prev_preorder().is_none());
    }
    assert!(forest.tree(TreeIx::from_raw(4)).is_none());
    let expected: Vec<_> = forest
        .trees()
        .map(|tree| describe(tree.root_node()))
        .collect();
    let core = forest.to_bytes().unwrap();
    let presence = forest.presence_cache().unwrap().as_bytes().to_vec();
    let points = forest.point_data().unwrap().as_bytes().to_vec();
    for mut loaded in [
        Forest::from_bytes(&languages, &core).unwrap(),
        Forest::from_retained(&languages, retain(&core)).unwrap(),
    ] {
        assert!(!loaded.has_points());
        loaded
            .set_presence_cache(PresenceCache::from_retained(retain(&presence)).unwrap())
            .unwrap();
        loaded
            .set_point_data(PointsData::from_bytes(&points).unwrap())
            .unwrap();
        assert_eq!(
            loaded
                .trees()
                .map(|tree| describe(tree.root_node()))
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(loaded.to_bytes().unwrap(), core);
    }
    let ids: Vec<_> = forest
        .trees()
        .flat_map(|tree| tree.preorder().nodes())
        .map(|node| node.id())
        .collect();
    forest.drop_presence_cache();
    forest.drop_point_data();
    assert_eq!(
        forest
            .trees()
            .flat_map(|tree| tree.preorder().nodes())
            .map(|node| node.id())
            .collect::<Vec<_>>(),
        ids
    );
    for tree in forest.trees() {
        assert!(!tree.has_points());
        assert_eq!(
            tree.start_position(),
            tree_sitter::Point::new(0, tree.start_byte())
        );
    }
    let (empty, mapping) = Packer::new()
        .unwrap()
        .pack_forest(Vec::new(), PackOptions::default())
        .unwrap();
    assert!(mapping.is_empty());
    assert_eq!(
        Forest::from_bytes(&[], empty.as_bytes())
            .unwrap()
            .trees()
            .count(),
        0
    );
}
