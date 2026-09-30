mod support;

use std::{collections::HashSet, ops::ControlFlow};
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
fn grammar_caches_follow_cursor_resets_and_forest_copies() {
    use tree_squatter::traits::NodeLike;

    let json = support::json_language();
    let c = support::c_language();
    let array = format!("[{}0]", "1,".repeat(100));
    let native = [
        support::parse_native(&json, &array),
        support::parse_native(&c, "int answer(int value) { return value + 1; }"),
        support::parse_native(&json, "{\"other\": true}"),
    ];
    let forests = {
        let languages = [
            Language::new(&json).unwrap(),
            Language::new(&c).unwrap(),
            Language::new(&json).unwrap(),
        ];
        let (packed, _) = Packer::new()
            .unwrap()
            .pack_forest(
                languages
                    .iter()
                    .zip(&native)
                    .map(|(language, tree)| PackRegion {
                        language: language.clone(),
                        roots: vec![tree.root_node()],
                    })
                    .collect::<Vec<_>>(),
                PackOptions::default(),
            )
            .unwrap();
        let mut loaded = Forest::from_bytes(&languages, packed.as_bytes()).unwrap();
        loaded
            .set_point_data(
                PointsData::from_bytes(packed.point_data().unwrap().as_bytes()).unwrap(),
            )
            .unwrap();
        [packed.detach().unwrap(), packed.repack().unwrap(), loaded]
    };
    let initial = forests[0].tree(TreeIx::from_raw(0)).unwrap().root_node();
    let mut cursor = initial.walk();
    for forest in &forests {
        for (tree, native) in forest.trees().zip(&native) {
            let nodes: Vec<_> = tree.preorder().nodes().collect();
            assert_eq!(
                std::iter::successors(Some(tree.root_node()), |node| node.next_preorder())
                    .collect::<Vec<_>>(),
                nodes
            );
            for (node, native) in nodes
                .iter()
                .copied()
                .zip(NodeLike::preorder(native.root_node()))
            {
                let expected = NodeLike::attributes(native);
                assert_eq!(node.attributes(), expected);
                cursor.reset(node);
                assert_eq!(cursor.attributes(), expected);
                assert_eq!(cursor.clone().attributes(), expected);
                cursor.reset(initial);
                cursor.reset_to(&node.walk());
                assert_eq!(cursor.attributes(), expected);
                if cursor.goto_first_child() {
                    assert_eq!(cursor.field_name(), cursor.node().field_name());
                }
            }
            let kinds = tree_squatter::KindSet::new(nodes.iter().map(|node| node.kind_id()));
            assert_eq!(
                tree.all()
                    .filter_kind_ids(&kinds)
                    .nodes()
                    .collect::<Vec<_>>(),
                nodes
            );

            cursor.reset(tree.root_node());
            let mut reference = native.walk();
            let mut visited = Vec::new();
            loop {
                visited.push(cursor.node());
                let saved = cursor.clone();
                cursor.reset(initial);
                cursor.reset_to(&saved);
                assert_eq!(cursor.node(), *visited.last().unwrap());
                assert_eq!(cursor.attributes(), NodeLike::attributes(reference.node()));
                assert_eq!(cursor.depth(), reference.depth());
                assert_eq!(
                    cursor.field_id().map(|field| field.raw()),
                    reference.field_id().map(|field| field.get())
                );
                assert_eq!(cursor.field_name(), reference.field_name());
                let descended = cursor.goto_first_child();
                assert_eq!(descended, reference.goto_first_child());
                if descended {
                    continue;
                }
                loop {
                    let advanced = cursor.goto_next_sibling();
                    assert_eq!(advanced, reference.goto_next_sibling());
                    if advanced {
                        break;
                    }
                    let ascended = cursor.goto_parent();
                    assert_eq!(ascended, reference.goto_parent());
                    if !ascended {
                        break;
                    }
                }
                if cursor.depth() == 0 {
                    break;
                }
            }
            assert_eq!(visited, nodes);
            assert_eq!(cursor.node(), tree.root_node());
            assert!(!cursor.goto_previous_sibling());
            assert!(!cursor.goto_next_sibling());
        }
    }
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
    let mut checks = 0;
    let canceled = PresenceCache::build_selected_with_cancellation(
        &forest,
        |_| true,
        || {
            checks += 1;
            if checks == forest.regions().count() + 2 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        },
    );
    assert!(matches!(
        canceled,
        Err(tree_squatter::SideDataError::Core(
            tree_squatter::Error::Canceled
        ))
    ));
    assert_eq!(forest.presence_cache().unwrap().as_bytes(), presence);
    assert!(matches!(
        Packer::new().unwrap().pack_forest(
            vec![PackRegion {
                language: languages[0].clone(),
                roots: vec![roots[0]],
            }],
            PackOptions {
                cancellation_callback: Some(&|| ControlFlow::Break(())),
                ..Default::default()
            },
        ),
        Err(tree_squatter::Error::Canceled)
    ));
    for mut loaded in [
        Forest::from_bytes(&languages, &core).unwrap(),
        Forest::from_retained(&languages, retain(&core)).unwrap(),
        forest.detach().unwrap(),
        forest.repack().unwrap(),
    ] {
        loaded.drop_point_data();
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

type MatchDescription = (usize, Vec<tree_squatter::NodeId>);

fn matches<'forest>(
    cursor: &mut tree_squatter::QueryCursor,
    query: &tree_squatter::Query,
    scope: impl Into<tree_squatter::QueryScope<'forest>>,
    sources: &[&[u8]],
) -> Vec<MatchDescription> {
    let provider = |node: Node<'_>| {
        let source = sources[node.id().tree().raw() as usize];
        std::iter::once(&source[node.byte_range()])
    };
    let mut execution = cursor.execute(query, scope, provider);
    let mut results = Vec::new();
    let mut identities = HashSet::new();
    while let Some(found) = execution.next_match() {
        assert!(identities.insert(found.id()));
        results.push((
            found.pattern_index.raw(),
            found
                .captures()
                .iter()
                .map(|capture| capture.node.id())
                .collect(),
        ));
    }
    assert!(execution.error().is_none());
    results
}

#[test]
fn region_queries_select_sources_by_tree() {
    use tree_squatter::{Query, QueryCursor, QueryExecutionError};
    let c = support::c_language();
    let json = support::json_language();
    let language = Language::new(&c).unwrap();
    let sources = [
        b"int answer() { return 42; }".as_slice(),
        b"int answer = ;",
        b"int other;",
        b"[]",
        b"int answer;",
    ];
    let native: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(index, source)| support::parse_native(if index == 3 { &json } else { &c }, source))
        .collect();
    let (forest, _) = Packer::new()
        .unwrap()
        .pack_forest(
            vec![
                PackRegion {
                    language: language.clone(),
                    roots: native[..3].iter().map(|tree| tree.root_node()).collect(),
                },
                PackRegion {
                    language: Language::new(&json).unwrap(),
                    roots: vec![native[3].root_node()],
                },
                PackRegion {
                    language: language.clone(),
                    roots: vec![native[4].root_node()],
                },
            ],
            PackOptions {
                symbol_presence: &|_| true,
                ..Default::default()
            },
        )
        .unwrap();
    let query = Query::new(&language, "((identifier) @name (#eq? @name \"answer\"))").unwrap();
    let regions: Vec<_> = forest.regions().collect();
    for optimized in [false, true] {
        let mut cursor = QueryCursor::new();
        cursor.set_optimized(optimized);
        for region in [regions[0], regions[2]] {
            let expected: Vec<_> = region
                .trees()
                .flat_map(|tree| matches(&mut cursor, &query, tree, &sources))
                .collect();
            assert_eq!(matches(&mut cursor, &query, region, &sources), expected);
            assert!(!expected.is_empty());
        }
        let mut wrong_language = cursor.execute(&query, regions[1], sources[3]);
        assert_eq!(
            wrong_language.error(),
            Some(QueryExecutionError::InvalidExecution)
        );
        assert!(wrong_language.next_match().is_none());
    }
    let mut cursor = QueryCursor::new();
    let provider = |node: Node<'_>| {
        std::iter::once(&sources[node.id().tree().raw() as usize][node.byte_range()])
    };
    let mut execution = cursor.execute(&query, regions[0], provider);
    let mut removed = None;
    let mut identities = HashSet::new();
    while let Some((found, _)) = execution.next_capture() {
        assert_ne!(Some(found.id()), removed);
        identities.insert(found.id());
        if removed.is_none() {
            removed = Some(found.id());
            found.remove();
        }
    }
    assert!(identities.len() >= 2);
}

#[test]
fn bounded_region_queries_preserve_ordering_semantics() {
    use tree_squatter::{Query, QueryCursor};
    let json = support::json_language();
    let language = Language::new(&json).unwrap();
    let examples = [
        (vec![0..20, 30..40, 50..70], 35..55, vec![1, 2]),
        (
            vec![0..1000, 20..40, 500..600, 700..800],
            510..520,
            vec![0, 2],
        ),
        (
            vec![500..600, 700..800, 20..40, 0..1000],
            510..520,
            vec![0, 3],
        ),
    ];
    for (bounds, viewport, selected) in examples {
        let text: Vec<_> = bounds
            .iter()
            .map(|range| format!("\"{}\"", " ".repeat(range.len() - 2)))
            .collect();
        let native: Vec<_> = text
            .iter()
            .map(|source| support::parse_native(&json, source))
            .collect();
        let roots: Vec<_> = native
            .iter()
            .zip(&bounds)
            .map(|(tree, range)| {
                tree.root_node_with_offset(range.start, tree_sitter::Point::new(0, range.start))
            })
            .collect();
        let sources: Vec<_> = text
            .iter()
            .zip(&bounds)
            .map(|(source, range)| format!("{}{}", " ".repeat(range.start), source).into_bytes())
            .collect();
        let sources: Vec<_> = sources.iter().map(Vec::as_slice).collect();
        let (forest, _) = Packer::new()
            .unwrap()
            .pack_forest(
                vec![PackRegion {
                    language: language.clone(),
                    roots,
                }],
                PackOptions {
                    symbol_presence: &|_| true,
                    ..Default::default()
                },
            )
            .unwrap();
        let loaded =
            Forest::from_bytes(std::slice::from_ref(&language), forest.as_bytes()).unwrap();
        let query = Query::new(&language, "(document (string) @value) @root").unwrap();
        for forest in [&forest, &loaded] {
            for optimized in [false, true] {
                for points in [false, true] {
                    let mut cursor = QueryCursor::new();
                    cursor.set_optimized(optimized);
                    if points {
                        cursor.set_point_range(
                            tree_sitter::Point::new(0, viewport.start)
                                ..tree_sitter::Point::new(0, viewport.end),
                        );
                    } else {
                        cursor.set_byte_range(viewport.clone());
                    }
                    let expected: Vec<_> = forest
                        .trees()
                        .flat_map(|tree| matches(&mut cursor, &query, tree, &sources))
                        .collect();
                    let region = forest.regions().next().unwrap();
                    let actual = matches(&mut cursor, &query, region, &sources);
                    assert_eq!(actual, expected);
                    assert_eq!(
                        actual
                            .iter()
                            .map(|(_, captures)| captures[0].tree().raw())
                            .collect::<Vec<_>>(),
                        selected
                    );
                }
            }
        }
    }
    // Empty roots at the viewport start must survive the nonoverlapping seek.
    let c = support::c_language();
    let language = Language::new(&c).unwrap();
    let empty = support::parse_native(&c, "");
    let root = empty.root_node_with_offset(10, tree_sitter::Point::new(0, 10));
    let (forest, _) = Packer::new()
        .unwrap()
        .pack_forest(
            vec![PackRegion {
                language: language.clone(),
                roots: vec![root, root],
            }],
            PackOptions::default(),
        )
        .unwrap();
    let query = Query::new(&language, "(translation_unit) @root").unwrap();
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(10..11);
    assert_eq!(
        matches(
            &mut cursor,
            &query,
            forest.regions().next().unwrap(),
            &[b"          ", b"          "]
        )
        .len(),
        2
    );
}
