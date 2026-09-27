use tree_squatter::{Language, PackContext, PackOptions, Tree};

#[test]
fn slab_headers_reject_incompatible_formats() {
    use tree_squatter::{PointData, PresenceCache};

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
                _ => PointData::copy_from_bytes(&tree, &invalid).is_err(),
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
            context.trim();
        }
    }
}

#[test]
fn side_data_changes_only_attached_coordinates() {
    use tree_squatter::{LineIndex, PointData, PresenceCache};
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
                points: false,
                symbol_presence: false,
                ..PackOptions::default()
            },
        )
        .unwrap();
        let core_address = tree.as_bytes().as_ptr();
        let core_bytes = tree.as_bytes().to_vec();
        let root_slot = tree.root_node().slot();
        let line_index = LineIndex::new(source.as_bytes()).unwrap();
        assert_eq!(line_index.point(0), tree_sitter::Point::new(0, 0));

        let expected = tree
            .root_node()
            .preorder()
            .nodes()
            .map(|node| {
                (
                    node.slot(),
                    line_index.point(node.start_byte()),
                    line_index.point(node.end_byte()),
                )
            })
            .collect::<Vec<_>>();
        assert!(!tree.root_node().has_points());
        assert_eq!(
            tree.root_node().start_position(),
            tree_sitter::Point::new(0, tree.root_node().start_byte())
        );
        let presence =
            std::thread::scope(|scope| scope.spawn(|| PresenceCache::build(&tree)).join().unwrap())
                .unwrap();
        let points = PointData::build(&tree, &line_index).unwrap();
        tree.set_presence_cache(presence).unwrap();
        tree.set_point_data(points).unwrap();
        drop(line_index);
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
    use tree_squatter::{LineIndex, PointData, PresenceCache, StableSlab};

    struct Backing {
        words: Box<[u64]>,
        drops: Arc<AtomicUsize>,
    }
    impl Drop for Backing {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    unsafe impl StableSlab for Backing {
        fn bytes(&self) -> &[u8] {
            unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.words.len() * 8) }
        }
    }
    struct MisalignedBacking(Box<[u64]>, usize);
    unsafe impl StableSlab for MisalignedBacking {
        fn bytes(&self) -> &[u8] {
            unsafe { std::slice::from_raw_parts(self.0.as_ptr().cast::<u8>().add(1), self.1) }
        }
    }
    fn backing(bytes: &[u8], drops: Arc<AtomicUsize>) -> Backing {
        let mut words = vec![0u64; bytes.len() / 8].into_boxed_slice();
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), words.as_mut_ptr().cast(), bytes.len());
        }
        Backing { words, drops }
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
            points: false,
            symbol_presence: false,
            ..PackOptions::default()
        },
    )
    .unwrap();
    let points = PointData::build(&tree, &LineIndex::new(source.as_bytes()).unwrap()).unwrap();
    let presence = PresenceCache::build(&tree).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let mapped_backing = backing(points.as_bytes(), drops.clone());
    let mapped_address = mapped_backing.bytes().as_ptr();
    let mapped = PointData::from_backing(&tree, mapped_backing).unwrap();
    assert_eq!(mapped.as_bytes().as_ptr(), mapped_address);
    tree.set_point_data(mapped).unwrap();
    assert!(
        PointData::from_backing(
            &tree,
            MisalignedBacking(
                vec![0; points.as_bytes().len() / 8 + 1].into_boxed_slice(),
                points.as_bytes().len()
            )
        )
        .is_err()
    );
    let copied = PresenceCache::copy_from_bytes(&tree, presence.as_bytes()).unwrap();
    tree.set_presence_cache(copied).unwrap();
    let presence_drops = Arc::new(AtomicUsize::new(0));
    let mapped_backing = backing(presence.as_bytes(), presence_drops.clone());
    let mapped_address = mapped_backing.bytes().as_ptr();
    let mapped = PresenceCache::from_backing(&tree, mapped_backing).unwrap();
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
            PointData::build(&other, &LineIndex::new(other_source.as_bytes()).unwrap()).unwrap()
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
    assert!(PointData::copy_from_bytes(&tree, &invalid).is_err());
    let mut invalid = points.as_bytes().to_vec();
    let root_start = 16 + tree.root_node().slot().get() as usize * 16;
    invalid[root_start..root_start + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(
        PointData::copy_from_bytes(&tree, &invalid).is_err(),
        cfg!(debug_assertions)
    );
    assert_eq!(tree.root_node().start_position(), original_point);
    tree.drop_point_data();
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(tree.presence_cache().is_some());
    tree.drop_presence_cache();
    assert_eq!(presence_drops.load(Ordering::Relaxed), 1);
    let copied_points = PointData::copy_from_bytes(&tree, points.as_bytes()).unwrap();
    tree.set_point_data(copied_points).unwrap();
    assert_eq!(tree.root_node().start_position(), original_point);

    let core_drops = Arc::new(AtomicUsize::new(0));
    let owner = backing(tree.as_bytes(), core_drops.clone());
    let core_address = owner.bytes().as_ptr();
    let mut backed = Tree::from_owned_slab(&grammar, owner).unwrap();
    backed
        .set_presence_cache(PresenceCache::build(&backed).unwrap())
        .unwrap();
    backed
        .set_point_data(PointData::copy_from_bytes(&backed, points.as_bytes()).unwrap())
        .unwrap();
    assert_eq!(backed.root_node().start_position(), original_point);
    backed.drop_presence_cache();
    backed.drop_point_data();
    assert!(!backed.has_points());
    assert!(backed.presence_cache().is_none());
    assert_eq!(backed.as_bytes().as_ptr(), core_address);
    assert_eq!(core_drops.load(Ordering::Relaxed), 0);
    drop(backed);
    assert_eq!(core_drops.load(Ordering::Relaxed), 1);
}

#[test]
fn line_index_is_byte_based() {
    use tree_squatter::{LineIndex, PointData};

    let source = b"\xef\xbb\xbfa\r\n\xc3\xa9\n";
    let index = LineIndex::new(source).unwrap();
    for (byte, point) in [
        (0, tree_sitter::Point::new(0, 0)),
        (3, tree_sitter::Point::new(0, 3)),
        (5, tree_sitter::Point::new(0, 5)),
        (6, tree_sitter::Point::new(1, 0)),
        (8, tree_sitter::Point::new(1, 2)),
        (9, tree_sitter::Point::new(2, 0)),
        (10, tree_sitter::Point::new(2, 1)),
        (usize::MAX, tree_sitter::Point::new(2, usize::MAX - 9)),
    ] {
        assert_eq!(index.point(byte), point);
    }
    assert_eq!(
        LineIndex::new(b"").unwrap().point(usize::MAX),
        tree_sitter::Point::new(0, usize::MAX)
    );
    let index = {
        let source = String::from("a\nbc");
        LineIndex::new(source.as_bytes()).unwrap()
    };
    assert_eq!(index.point(7), tree_sitter::Point::new(1, 5));
    assert_eq!(
        LineIndex::new(b"").unwrap().point(0),
        tree_sitter::Point::new(0, 0)
    );

    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse("[1]", None).unwrap();
    let mut tree = Tree::pack_with_options(
        &grammar,
        &native,
        PackOptions {
            points: false,
            symbol_presence: false,
            ..PackOptions::default()
        },
    )
    .unwrap();
    assert!(PointData::build(&tree, &LineIndex::new(b"[").unwrap()).is_ok());
    assert!(!tree.has_points());
    assert!(tree.presence_cache().is_none());
    tree.drop_presence_cache();
    tree.drop_point_data();
}

#[test]
fn point_bounded_queries_follow_attachment() {
    use tree_squatter::{LineIndex, PointData, Query, QueryCursor};
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
            points: false,
            ..PackOptions::default()
        },
    )
    .unwrap();
    let query = Query::new(&grammar, "(number) @number").unwrap();
    let mut cursor = QueryCursor::new();
    let mut count = |tree: &Tree| {
        assert!(
            cursor.set_point_range(tree_sitter::Point::new(1, 0)..tree_sitter::Point::new(2, 0))
        );
        let mut execution = cursor.execute(&query, tree.root_node(), source.as_slice());
        let mut found = 0;
        while execution.next_match().is_some() {
            found += 1;
        }
        found
    };
    assert_eq!(count(&tree), 0);
    let points = PointData::build(&tree, &LineIndex::new(source).unwrap()).unwrap();
    tree.set_point_data(points).unwrap();
    assert_eq!(count(&tree), 1);
    tree.drop_point_data();
    assert_eq!(count(&tree), 0);
}

#[test]
fn side_data_creation_flags_do_not_change_core_layout() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let source = format!("[{}0]", "1,\n".repeat(500));
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse(&source, None).unwrap();
    let mut baseline = None;
    for points in [false, true] {
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
