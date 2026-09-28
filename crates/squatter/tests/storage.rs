use tree_squatter::{Language, PackContext, PackOptions, Tree};

#[test]
fn slab_headers_reject_incompatible_formats() {
    use tree_squatter::{PointsData, PresenceCache};

    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse("[1]", None).unwrap();
    let tree = Tree::pack(&grammar, &native).unwrap();
    let presence = tree.presence_cache().unwrap();
    let points = tree.point_data().unwrap();

    for (bytes, expected, flags) in [
        (tree.as_bytes(), 0xff00_0000, 0x3f),
        (presence.as_bytes(), 0xfe00_0000, 0),
        (points.as_bytes(), 0xfd00_0000, 0),
    ] {
        let header = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        assert_eq!(header & !flags, expected);
        for bit in 0..32 {
            if flags & (1 << bit) != 0 {
                continue;
            }
            let mut invalid = bytes.to_vec();
            invalid[..4].copy_from_slice(&(header ^ (1 << bit)).to_le_bytes());
            let rejected = match expected {
                0xff00_0000 => Tree::from_bytes(&grammar, &invalid).is_err(),
                0xfe00_0000 => PresenceCache::copy_from_bytes(&tree, &invalid).is_err(),
                _ => PointsData::copy_from_bytes(&tree, &invalid).is_err(),
            };
            assert!(rejected, "format {expected:#x}, bit {bit}");
        }
    }
}

#[test]
fn packing_context_matches_fresh_packing_and_loading() {
    let json =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let c = unsafe { tree_sitter::Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) };
    let c_sharp = unsafe {
        tree_sitter::Language::from_raw(tree_sitter_c_sharp::LANGUAGE.into_raw()().cast())
    };
    let wide = format!("[{}0]", "{\"a\": 1, \"b\": true},\n".repeat(1000));
    let deep = format!("{}0{}", "[".repeat(300), "]".repeat(300));
    let sibling_depths = (29..=33)
        .map(|depth| format!("{}0{}", "[".repeat(depth), "]".repeat(depth)))
        .collect::<Vec<_>>()
        .join(",");
    let inline_boundary = format!("[{sibling_depths}]");
    let long = format!("[\"{}\",\n\"{}\"]", "a".repeat(70000), "b".repeat(400));

    for (language, sources) in [
        (
            json,
            vec![
                "0",
                "{}",
                "{\"a\": [1, true, null]}",
                "[1,",
                &wide,
                &deep,
                &inline_boundary,
                &long,
            ],
        ),
        (
            c,
            vec![
                "",
                "int f(int x) { /* extra */ return x + 1; }",
                "int x = ;",
                "int f() { return 1 }",
            ],
        ),
        (
            c_sharp,
            vec![
                "class C { int F(int x) => x + 1; }",
                "class C { int x = ; }",
            ],
        ),
    ] {
        let grammar = Language::new(&language).unwrap();
        let fresh_grammar = tree_squatter::Language::new(&language).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let mut context = PackContext::new().unwrap();

        for source in sources {
            let tree = parser.parse(source, None).unwrap();
            for points in [false, true] {
                for symbol_presence in [false, true] {
                    for repack in [false, true] {
                        for initial_group_capacity in [0, 1] {
                            let options = PackOptions {
                                initial_group_capacity,
                                repack,
                                symbol_presence,
                                points,
                            };
                            let expected =
                                Tree::pack_with_options(&fresh_grammar, &tree, options).unwrap();
                            let actual =
                                context.pack_with_options(&grammar, &tree, options).unwrap();
                            assert_eq!(actual.has_points(), points);
                            assert_eq!(actual.presence_cache().is_some(), symbol_presence);
                            assert_eq!(
                                actual.point_data().map(|points| points.as_bytes()),
                                expected.point_data().map(|points| points.as_bytes())
                            );
                            assert_eq!(
                                actual.presence_cache().map(|cache| cache.as_bytes()),
                                expected.presence_cache().map(|cache| cache.as_bytes())
                            );
                            let description = format!("{} bytes, {options:?}", source.len());
                            assert_eq!(
                                actual.as_bytes().len(),
                                expected.as_bytes().len(),
                                "{description}"
                            );
                            assert!(
                                actual.as_bytes() == expected.as_bytes(),
                                "different slab: {description}, first difference {:?}",
                                actual
                                    .as_bytes()
                                    .iter()
                                    .zip(expected.as_bytes())
                                    .position(|(a, b)| a != b)
                            );

                            let loaded = Tree::from_bytes(&grammar, expected.as_bytes()).unwrap();
                            assert!(!loaded.has_points());
                            assert!(loaded.presence_cache().is_none());
                            let borrowed =
                                Tree::from_bytes_borrowed(&grammar, expected.as_bytes()).unwrap();
                            assert_eq!(loaded.as_bytes(), borrowed.as_bytes());
                            tree_squatter::Tree::from_bytes(&fresh_grammar, actual.as_bytes())
                                .unwrap();

                            let compact = actual.repack().unwrap();
                            assert_eq!(compact.has_points(), points);
                            assert_eq!(compact.presence_cache().is_some(), symbol_presence);
                            assert_eq!(compact.as_bytes(), expected.repack().unwrap().as_bytes());
                            for group in 0..actual.group_count() {
                                for symbol in 0..language.node_kind_count() as u16 {
                                    assert_eq!(
                                        actual.group_has_symbol(
                                            group,
                                            tree_squatter::KindId::new(symbol)
                                        ),
                                        expected.group_has_symbol(
                                            group,
                                            tree_squatter::KindId::new(symbol)
                                        )
                                    );
                                }
                            }
                        }
                    }
                }
            }
            context.drop_scratch();
        }
    }
}

#[test]
fn side_data_changes_only_attached_coordinates() {
    use tree_squatter::{PointsData, PresenceCache};
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();

    for source in ["", "[\n1,\n2]", "[\r\n\"é\"]\n", "[1,"] {
        let native = parser.parse(source, None).unwrap();
        let mut tree = Tree::pack_with_options(
            &grammar,
            &native,
            PackOptions {
                points: true,
                symbol_presence: false,
                ..PackOptions::default()
            },
        )
        .unwrap();
        let core_address = tree.as_bytes().as_ptr();
        let core_bytes = tree.as_bytes().to_vec();
        let root_slot = tree.root_node().slot();
        let points = tree.point_data().unwrap().as_bytes().to_vec();

        let expected = tree
            .root_node()
            .preorder()
            .nodes()
            .map(|node| (node.slot(), node.start_position(), node.end_position()))
            .collect::<Vec<_>>();
        tree.drop_point_data();
        assert!(!tree.root_node().has_points());
        assert_eq!(
            tree.root_node().start_position(),
            tree_sitter::Point::new(0, tree.root_node().start_byte())
        );
        let presence =
            std::thread::scope(|scope| scope.spawn(|| PresenceCache::build(&tree)).join().unwrap())
                .unwrap();
        let points = PointsData::copy_from_bytes(&tree, &points).unwrap();
        tree.set_presence_cache(presence).unwrap();
        tree.set_point_data(points).unwrap();
        assert!(tree.has_points());
        assert!(tree.root_node().attributes().has_points);
        for (slot, start, end) in expected {
            let node = tree.node_at_slot(slot).unwrap();
            assert_eq!(node.start_position(), start);
            assert_eq!(node.end_position(), end);
        }
        assert_eq!(tree.as_bytes().as_ptr(), core_address);
        assert_eq!(tree.as_bytes(), core_bytes);
        assert_eq!(tree.root_node().slot(), root_slot);
        tree.drop_presence_cache();
        assert!(tree.has_points());
        tree.drop_point_data();
        tree.drop_point_data();
        assert!(!tree.has_points());
        assert_eq!(
            tree.root_node().start_position(),
            tree_sitter::Point::new(0, tree.root_node().start_byte())
        );
        assert_eq!(tree.as_bytes().as_ptr(), core_address);
        assert_eq!(tree.as_bytes(), core_bytes);
    }
}

#[test]
fn sidecar_mapping_copy_and_failed_replacement() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tree_squatter::{PointsData, PresenceCache, StableSlab};

    struct SlabOwner {
        words: Box<[u64]>,
        drops: Arc<AtomicUsize>,
    }
    impl Drop for SlabOwner {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    unsafe impl StableSlab for SlabOwner {
        fn bytes(&self) -> &[u8] {
            unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.words.len() * 8) }
        }
    }
    struct MisalignedSlab(Box<[u64]>, usize);
    unsafe impl StableSlab for MisalignedSlab {
        fn bytes(&self) -> &[u8] {
            unsafe { std::slice::from_raw_parts(self.0.as_ptr().cast::<u8>().add(1), self.1) }
        }
    }
    fn slab_owner(bytes: &[u8], drops: Arc<AtomicUsize>) -> SlabOwner {
        let mut words = vec![0u64; bytes.len() / 8].into_boxed_slice();
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), words.as_mut_ptr().cast(), bytes.len());
        }
        SlabOwner { words, drops }
    }

    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let source = "[\n1, 2]";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse(source, None).unwrap();
    let mut tree = Tree::pack_with_options(
        &grammar,
        &native,
        PackOptions {
            points: true,
            symbol_presence: false,
            ..PackOptions::default()
        },
    )
    .unwrap();
    let points = PointsData::copy_from_bytes(&tree, tree.point_data().unwrap().as_bytes()).unwrap();
    tree.drop_point_data();
    let presence = PresenceCache::build(&tree).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let mapped_owner = slab_owner(points.as_bytes(), drops.clone());
    let mapped_address = mapped_owner.bytes().as_ptr();
    let mapped = PointsData::from_retained(&tree, mapped_owner).unwrap();
    assert_eq!(mapped.as_bytes().as_ptr(), mapped_address);
    tree.set_point_data(mapped).unwrap();
    assert!(
        PointsData::from_retained(
            &tree,
            MisalignedSlab(
                vec![0; points.as_bytes().len() / 8 + 1].into_boxed_slice(),
                points.as_bytes().len()
            )
        )
        .is_err()
    );
    let copied = PresenceCache::copy_from_bytes(&tree, presence.as_bytes()).unwrap();
    tree.set_presence_cache(copied).unwrap();
    let presence_drops = Arc::new(AtomicUsize::new(0));
    let mapped_owner = slab_owner(presence.as_bytes(), presence_drops.clone());
    let mapped_address = mapped_owner.bytes().as_ptr();
    let mapped = PresenceCache::from_retained(&tree, mapped_owner).unwrap();
    assert_eq!(mapped.as_bytes().as_ptr(), mapped_address);
    tree.set_presence_cache(mapped).unwrap();
    let original_point = tree.root_node().start_position();
    let core_address = tree.as_bytes().as_ptr();
    let core_bytes = tree.as_bytes().to_vec();
    let point_address = tree.point_data().unwrap().as_bytes().as_ptr();
    let presence_address = tree.presence_cache().unwrap().as_bytes().as_ptr();
    let other_source = format!("[{}0]", "1,".repeat(500));
    let other_native = parser.parse(&other_source, None).unwrap();
    let other = Tree::pack(&grammar, &other_native).unwrap();
    assert_ne!(tree.group_count(), other.group_count());
    assert!(
        tree.set_presence_cache(PresenceCache::build(&other).unwrap())
            .is_err()
    );
    assert!(
        tree.set_point_data(
            PointsData::copy_from_bytes(&other, other.point_data().unwrap().as_bytes()).unwrap()
        )
        .is_err()
    );
    assert_eq!(tree.as_bytes().as_ptr(), core_address);
    assert_eq!(tree.as_bytes(), core_bytes);
    assert_eq!(
        tree.point_data().unwrap().as_bytes().as_ptr(),
        point_address
    );
    assert_eq!(
        tree.presence_cache().unwrap().as_bytes().as_ptr(),
        presence_address
    );
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert_eq!(presence_drops.load(Ordering::Relaxed), 0);
    let mut invalid = points.as_bytes().to_vec();
    invalid[4..8].copy_from_slice(&0u32.to_le_bytes());
    assert!(PointsData::copy_from_bytes(&tree, &invalid).is_err());
    let mut invalid = points.as_bytes().to_vec();
    let root_start = 16 + (tree.root_node().slot().get() as usize / 32) * (16 + 32 * 4);
    invalid[root_start..root_start + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    invalid[root_start + 16..root_start + 18].copy_from_slice(&1u16.to_le_bytes());
    assert!(PointsData::copy_from_bytes(&tree, &invalid).is_err());
    assert_eq!(tree.root_node().start_position(), original_point);
    tree.drop_point_data();
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(tree.presence_cache().is_some());
    tree.drop_presence_cache();
    assert_eq!(presence_drops.load(Ordering::Relaxed), 1);
    let copied_points = PointsData::copy_from_bytes(&tree, points.as_bytes()).unwrap();
    tree.set_point_data(copied_points).unwrap();
    assert_eq!(tree.root_node().start_position(), original_point);

    let core_drops = Arc::new(AtomicUsize::new(0));
    let owner = slab_owner(tree.as_bytes(), core_drops.clone());
    let core_address = owner.bytes().as_ptr();
    let mut retained = Tree::from_retained(&grammar, owner).unwrap();
    retained
        .set_presence_cache(PresenceCache::build(&retained).unwrap())
        .unwrap();
    retained
        .set_point_data(PointsData::copy_from_bytes(&retained, points.as_bytes()).unwrap())
        .unwrap();
    assert_eq!(retained.root_node().start_position(), original_point);
    retained.drop_presence_cache();
    retained.drop_point_data();
    assert!(!retained.has_points());
    assert!(retained.presence_cache().is_none());
    assert_eq!(retained.as_bytes().as_ptr(), core_address);
    assert_eq!(core_drops.load(Ordering::Relaxed), 0);
    drop(retained);
    assert_eq!(core_drops.load(Ordering::Relaxed), 1);
}

#[test]
fn point_bounded_queries_follow_attachment() {
    use tree_squatter::{PointsData, Query, QueryCursor};
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let source = b"[\n1,\n2]";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse(source, None).unwrap();
    let mut tree = Tree::pack_with_options(
        &grammar,
        &native,
        PackOptions {
            points: true,
            ..PackOptions::default()
        },
    )
    .unwrap();
    let query = Query::new(&grammar, "(number) @number").unwrap();
    let mut cursor = QueryCursor::new();
    let mut count = |tree: &Tree, containing| {
        let range = tree_sitter::Point::new(1, 0)..tree_sitter::Point::new(2, 0);
        let unbounded = tree_sitter::Point::new(0, 0)..tree_sitter::Point::new(0, 0);
        if containing {
            cursor
                .set_point_range(unbounded)
                .set_containing_point_range(range);
        } else {
            cursor
                .set_containing_point_range(unbounded)
                .set_point_range(range);
        }
        let mut execution = cursor.execute(&query, tree.root_node(), source.as_slice());
        let mut found = 0;
        while execution.next_match().is_some() {
            found += 1;
        }
        found
    };
    let points = PointsData::copy_from_bytes(&tree, tree.point_data().unwrap().as_bytes()).unwrap();
    tree.drop_point_data();
    assert_eq!(count(&tree, false), 0);
    assert_eq!(count(&tree, true), 0);
    tree.set_point_data(points).unwrap();
    assert_eq!(count(&tree, false), 1);
    assert_eq!(count(&tree, true), 1);
    tree.drop_point_data();
    assert_eq!(count(&tree, false), 0);
    assert_eq!(count(&tree, true), 0);
}

#[test]
fn presence_creation_does_not_change_core_layout() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let source = format!("[{}0]", "1,\n".repeat(500));
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse(&source, None).unwrap();
    for points in [false, true] {
        let mut baseline = None;
        for symbol_presence in [false, true] {
            let tree = Tree::pack_with_options(
                &grammar,
                &native,
                PackOptions {
                    points,
                    symbol_presence,
                    repack: true,
                    ..PackOptions::default()
                },
            )
            .unwrap();
            assert_eq!(tree.has_points(), points);
            assert_eq!(tree.presence_cache().is_some(), symbol_presence);
            if let Some(bytes) = &baseline {
                assert_eq!(tree.as_bytes(), bytes);
            } else {
                baseline = Some(tree.as_bytes().to_vec());
            }
        }
    }
}

#[test]
fn repacking_in_place_preserves_nodes_and_side_data() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse("[1,\n2, 3]", None).unwrap();
    let mut tree = Tree::pack_with_options(
        &grammar,
        &native,
        PackOptions {
            initial_group_capacity: 32,
            ..Default::default()
        },
    )
    .unwrap();
    let expected = tree.repack().unwrap();
    let presence = tree.presence_cache().unwrap().as_bytes().as_ptr();
    let points = tree.point_data().unwrap().as_bytes().as_ptr();
    tree.repack_in_place().unwrap();
    assert_eq!(tree.group_capacity(), tree.group_count());
    assert_eq!(tree.presence_cache().unwrap().as_bytes().as_ptr(), presence);
    assert_eq!(tree.point_data().unwrap().as_bytes().as_ptr(), points);
    for (actual, expected) in tree
        .root_node()
        .preorder()
        .nodes()
        .zip(expected.root_node().preorder().nodes())
    {
        assert_eq!(actual.kind_id(), expected.kind_id());
        assert_eq!(actual.grammar_id(), expected.grammar_id());
        assert_eq!(actual.byte_range(), expected.byte_range());
        assert_eq!(actual.start_position(), expected.start_position());
        assert_eq!(actual.end_position(), expected.end_position());
    }
    Tree::from_bytes(&grammar, tree.as_bytes()).unwrap();
    tree.repack_in_place().unwrap();
}
