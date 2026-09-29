use super::*;
use crate::{GrammarId, PackedParseOptions, SlotIx, TreeFellerParser};
use std::ffi::c_void;

unsafe extern "C" {
    fn sq_test_dictionaries();
    fn sq_test_grammar_limits();
    fn sq_test_lexer_fallback();
    fn sq_test_chunked_lexer();
    fn sq_test_external_lexer();
    fn sq_test_unsupported_parsers();
    fn sq_test_parser_language() -> *const c_void;
    fn sq_test_external_language(branches: bool) -> *const c_void;
    fn sq_test_ambiguous_language() -> *const c_void;
    fn sq_test_supertypes(count: u32, connected: bool) -> *const c_void;
    fn sq_test_supertypes_delete(language: *const c_void);
    fn sq_test_symbols(count: u32) -> *const c_void;
    fn sq_test_compact_symbols(count: u32) -> *const c_void;
    fn sq_test_symbols_delete(language: *const c_void);
}

struct LanguageOwner(*const c_void, unsafe extern "C" fn(*const c_void));

impl Drop for LanguageOwner {
    fn drop(&mut self) {
        unsafe { (self.1)(self.0) };
    }
}

struct Fixture {
    grammar: Language,
    // The synthetic language must outlive every prepared grammar and tree.
    _owner: LanguageOwner,
}

fn synthetic_forest(language: &Language, groups: u32) -> Result<Forest, Error> {
    let mut forest = Forest::empty(std::slice::from_ref(language), groups)?;
    forest.data_mut().trees.push(TreeData {
        region: RegionIx(0),
        slots: SlotIx(0)..SlotIx(groups * GROUP_SIZE),
    });
    forest.data_mut().regions[0].slots.end = SlotIx(groups * GROUP_SIZE);
    forest.data_mut().regions[0].trees.end = TreeIx(1);
    Ok(forest)
}

fn put_span_delta(data: &mut ForestData, slot: u32, value: u16) {
    if SPAN_BITS == 16 {
        data.put_short(data.layout.span_delta, slot, value);
    } else {
        data.put_byte(data.layout.span_delta, slot, value as u8);
    }
}

impl Fixture {
    unsafe fn new(pointer: *const c_void, delete: unsafe extern "C" fn(*const c_void)) -> Self {
        let language = unsafe { tree_sitter::Language::from_raw(pointer.cast()) };
        Self {
            grammar: Language::new(&language).unwrap(),
            _owner: LanguageOwner(pointer, delete),
        }
    }

    fn supertypes(count: u32, connected: bool) -> Self {
        unsafe {
            Self::new(
                sq_test_supertypes(count, connected),
                sq_test_supertypes_delete,
            )
        }
    }

    fn symbols(count: u32) -> Self {
        unsafe { Self::new(sq_test_symbols(count), sq_test_symbols_delete) }
    }
}

#[test]
fn synthetic_grammar_dictionaries_aliases_and_limits() {
    unsafe {
        sq_test_dictionaries();
        sq_test_grammar_limits();
        sq_test_unsupported_parsers();
    }
}

#[test]
fn compatible_parser_preserves_language_after_failed_selection() {
    let native = unsafe { tree_sitter::Language::from_raw(sq_test_parser_language().cast()) };
    let language = Language::new(&native).unwrap();
    let unparseable = Fixture::symbols(3);
    let mut parser = crate::Parser::new();
    assert!(matches!(
        parser.set_language(&unparseable.grammar),
        Err(tree_sitter::LanguageError::NotParseable)
    ));
    assert!(parser.language().is_none());
    parser.set_language(&language).unwrap();
    assert!(parser.set_language(&unparseable.grammar).is_err());
    assert_eq!(parser.language().unwrap().tree_sitter_language(), native);
    assert_eq!(parser.parse("x").unwrap().root_node().byte_range(), 0..1);
}

#[test]
fn callback_input_during_ambiguity_replay() {
    let native = unsafe { tree_sitter::Language::from_raw(sq_test_ambiguous_language().cast()) };
    let language = Language::new(&native).unwrap();
    let mut parser = TreeFellerParser::new(&language).unwrap();
    let source = b"x + x + x + x\n";
    let expected = parser.parse(source).unwrap();
    for chunk_size in 1..=8 {
        let mut maximum = 0;
        let mut replays = 0;
        let actual = parser
            .parse_with_options(
                &mut |byte, _| {
                    if byte == 0 && maximum > 0 {
                        replays += 1;
                    }
                    maximum = maximum.max(byte);
                    source[byte..(byte + chunk_size).min(source.len())].to_vec()
                },
                PackedParseOptions::default(),
            )
            .unwrap();
        assert!(replays > 0);
        assert_eq!(actual.as_bytes(), expected.as_bytes());
        assert_eq!(
            actual.point_data().unwrap().as_bytes(),
            expected.point_data().unwrap().as_bytes()
        );
    }
    let mut maximum = 0;
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        parser.parse_with_options(
            &mut |byte, _| {
                assert!(byte != 0 || maximum == 0, "panic during replay");
                maximum = maximum.max(byte);
                source[byte..(byte + 1).min(source.len())].to_vec()
            },
            PackedParseOptions::default(),
        )
    }));
    assert!(panic.is_err());
    assert_eq!(
        parser.parse(source).unwrap().as_bytes(),
        expected.as_bytes()
    );
}

#[test]
fn external_scanners_preserve_state_across_branches_and_replay() {
    unsafe { sq_test_external_lexer() };
    for (branches, source) in [
        (false, &b"a + b + c + d + a + b + c + d\n"[..]),
        (true, &b"a:x!"[..]),
    ] {
        let pointer = unsafe { sq_test_external_language(branches) };
        let fixture = unsafe { Fixture::new(pointer, sq_test_supertypes_delete) };
        let mut mainline = tree_sitter::Parser::new();
        mainline
            .set_language(&fixture.grammar.tree_sitter_language())
            .unwrap();
        let native = mainline.parse(source, None).unwrap();
        assert!(!native.root_node().has_error());
        let expected = Forest::pack(&fixture.grammar, &native).unwrap();
        let mut parser = TreeFellerParser::new(&fixture.grammar).unwrap();
        for chunk_size in 1..=8 {
            let mut maximum = 0;
            let mut replays = 0;
            let actual = parser
                .parse_with_options(
                    &mut |byte, _| {
                        if byte == 0 && maximum > 0 {
                            replays += 1;
                        }
                        maximum = maximum.max(byte);
                        source[byte..(byte + chunk_size).min(source.len())].to_vec()
                    },
                    PackedParseOptions::default(),
                )
                .unwrap();
            if !branches {
                assert!(replays > 0);
            }
            assert_eq!(actual.as_bytes(), expected.as_bytes());
            assert_eq!(
                actual.point_data().unwrap().as_bytes(),
                expected.point_data().unwrap().as_bytes()
            );
            assert!(parser.parse("invalid").is_err());
            assert_eq!(
                parser.parse(source).unwrap().as_bytes(),
                expected.as_bytes()
            );
        }
    }
}

#[test]
fn lexer_fallback_and_concurrent_parser_preparation() {
    unsafe {
        sq_test_lexer_fallback();
        sq_test_chunked_lexer();
    }
    let language = unsafe { tree_sitter::Language::from_raw(sq_test_parser_language().cast()) };
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse("\nx", None).unwrap();
    let options = PackOptions {
        initial_group_capacity: 1,
        repack: true,
        ..Default::default()
    };
    let expected = Forest::pack_with_options(&grammar, &native, options).unwrap();
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let grammar = grammar.clone();
                barrier.wait();
                for _ in 0..4 {
                    let mut parser = TreeFellerParser::new(&grammar).unwrap();
                    assert!(parser.parse("?").is_err());
                    let check = |parser: &mut TreeFellerParser| {
                        assert_eq!(
                            parser
                                .parse_with_options(
                                    &mut |byte, _| &b"\nx"[byte..],
                                    PackedParseOptions {
                                        pack: PackOptions {
                                            initial_group_capacity: 1,
                                            repack: true,
                                            ..Default::default()
                                        },
                                        ..Default::default()
                                    }
                                )
                                .unwrap()
                                .as_bytes(),
                            expected.as_bytes()
                        );
                    };
                    check(&mut parser);
                    parser.drop_scratch();
                    check(&mut parser);
                }
            });
        }
    });
}

fn leaf(symbol: u16, grammar: u16, supertype: u16, flags: u8) -> InputNode {
    InputNode {
        start_byte: 0,
        end_byte: 0,
        start_point: Point { row: 0, column: 0 },
        end_point: Point { row: 0, column: 0 },
        symbol: SquatterKindId(symbol),
        grammar: SquatterGrammarId(grammar),
        field: None,
        supertype,
        flags: flags.into(),
    }
}

#[test]
fn point_delta_limits_control_grouping() {
    let fixture = Fixture::symbols(16);
    for component in 0..4 {
        for difference in [255, 256] {
            let mut first = leaf(1, 1, 0, 1);
            first.start_point = Point {
                row: 100,
                column: 100,
            };
            first.end_point = Point {
                row: 1000,
                column: 1000,
            };
            let mut second = leaf(1, 1, 0, 0);
            second.start_point = first.start_point;
            second.end_point = first.end_point;
            match component {
                0 => first.start_point.row += difference,
                1 => first.start_point.column += difference,
                2 => first.end_point.row += difference,
                _ => first.end_point.column += difference,
            }
            let mut root = leaf(1, 1, 0, 1);
            root.start_point = second.start_point;
            root.end_point = first.end_point;
            for points in [false, true] {
                let mut builder = Builder::new(&fixture.grammar, 1, points).unwrap();
                builder.emit(&first, builder.distance()).unwrap();
                builder.emit(&second, builder.distance()).unwrap();
                builder.emit(&root, 0).unwrap();
                let tree = builder
                    .finish(PackOptions {
                        points,
                        ..Default::default()
                    })
                    .unwrap();
                assert_eq!(tree.group_count() == 1, !points || difference == 255);
                let nodes: Vec<_> = tree.root_node().preorder().nodes().collect();
                assert_eq!(nodes.len(), 3);
                if points {
                    assert_eq!(
                        tree.point_data().unwrap().as_bytes().len(),
                        16 + tree.group_count() as usize * (16 + GROUP_SIZE as usize * 4)
                    );
                    for (node, input) in nodes.iter().zip([&root, &second, &first]) {
                        assert_eq!(
                            node.start_position(),
                            tree_sitter::Point::new(
                                input.start_point.row as usize,
                                input.start_point.column as usize
                            )
                        );
                        assert_eq!(
                            node.end_position(),
                            tree_sitter::Point::new(
                                input.end_point.row as usize,
                                input.end_point.column as usize
                            )
                        );
                    }
                }
            }
        }
    }

    let mut builder = Builder::new(&fixture.grammar, 1, true).unwrap();
    let mut root = leaf(1, 1, 0, 1);
    root.start_point = Point {
        row: u32::MAX,
        column: u32::MAX,
    };
    root.end_point = root.start_point;
    builder.emit(&root, 0).unwrap();
    let tree = builder.finish(PackOptions::default()).unwrap();
    let expected = tree_sitter::Point::new(u32::MAX as usize, u32::MAX as usize);
    assert_eq!(tree.root_node().start_position(), expected);
    assert_eq!(tree.root_node().end_position(), expected);
    assert_eq!(
        tree.root_node()
            .all()
            .within_points(expected..expected)
            .count(),
        1
    );
}

fn check_masks(tree: &Forest, slots: &[SlotIx], bits: u32) {
    for (mask, &slot) in slots.iter().enumerate() {
        let node = tree.node_at_slot(slot).unwrap();
        for bit in 0..bits {
            assert_eq!(
                node.has_supertype(GrammarId::from_raw((bit + 2) as u16)),
                mask & (1 << bit) != 0
            );
        }
    }
}

#[test]
fn synthetic_supertype_emission_and_persistence() {
    for bits in 0..=9 {
        let fixture = Fixture::supertypes(bits, true);
        let grammar = &fixture.grammar;
        let mut counts = vec![1 << bits, (1 << bits).min(257)];
        counts.dedup();
        for count in counts {
            let mut builder = Builder::new(grammar, 1, true).unwrap();
            let mut slots = Vec::new();
            for mask in 0..count {
                builder
                    .emit(
                        &leaf(1, 1, mask as u16, u8::from(mask == 0)),
                        builder.distance(),
                    )
                    .unwrap();
                slots.push(SlotIx::from_raw(builder.distance() - 1));
            }
            builder.emit(&leaf(1, 1, 0, 1), 0).unwrap();
            let mut tree = builder.finish(PackOptions::default()).unwrap();
            let groups = tree.group_count();
            for capacity in [groups + 17, groups, groups + 1] {
                tree.resize(capacity, tree.data().flags()).unwrap();
                check_masks(&tree, &slots, bits);
                let loaded =
                    Forest::from_bytes(std::slice::from_ref(grammar), tree.as_bytes()).unwrap();
                let borrowed =
                    Forest::from_bytes_borrowed(std::slice::from_ref(grammar), tree.as_bytes())
                        .unwrap();
                check_masks(&loaded, &slots, bits);
                check_masks(&borrowed, &slots, bits);
                let restored = Language::from_cache(
                    &grammar.tree_sitter_language(),
                    &grammar.cache().unwrap(),
                )
                .unwrap();
                check_masks(
                    &Forest::from_bytes(std::slice::from_ref(&restored), tree.as_bytes()).unwrap(),
                    &slots,
                    bits,
                );
            }
            for (offset, mask) in [(0, 1), (14, 1)] {
                let mut bytes = tree.as_bytes().to_vec();
                bytes[offset] ^= mask;
                assert!(Forest::from_bytes(std::slice::from_ref(grammar), &bytes).is_err());
                assert!(
                    Forest::from_bytes_safety_checked(std::slice::from_ref(grammar), &bytes)
                        .is_err()
                );
            }
        }
    }
}

#[test]
fn id_width_covers_all_grammars_and_reserved_errors() {
    let small = Fixture::symbols(16);
    let boundary = Fixture::symbols(254);
    let wide = Fixture::symbols(255);
    assert_eq!(
        id_width_flags([&small.grammar, &boundary.grammar]),
        BYTE_IDS
    );
    assert_eq!(id_width_flags([&small.grammar, &wide.grammar]), 0);
    assert_eq!(id_width_flags([&wide.grammar, &small.grammar]), 0);
    for fixture in [&boundary, &wide] {
        let count = fixture.grammar.tables().symbol_count as u16;
        let mut builder = Builder::new(&fixture.grammar, 1, false).unwrap();
        builder
            .emit(&leaf(count + 1, count + 1, 0, 1 | 8), 0)
            .unwrap();
        builder.emit(&leaf(count, count, 0, 1 | 8), 0).unwrap();
        let tree = builder.finish(PackOptions::default()).unwrap();
        assert_eq!(
            tree.data().layout.symbol_width,
            if count == 254 { 1 } else { 2 }
        );
        let tree =
            Forest::from_bytes(std::slice::from_ref(&fixture.grammar), tree.as_bytes()).unwrap();
        assert_eq!(tree.data().symbol_index(0).raw(), count + 1);
        assert_eq!(tree.data().grammar_index(0).raw(), count + 1);
        assert_eq!(tree.data().symbol_index(1).raw(), count);
        assert_eq!(tree.data().grammar_index(1).raw(), count);
    }
}

#[test]
fn compact_domains_preserve_aliases_with_shared_width() {
    use crate::{KindId, SquatterGrammarId, SquatterKindId};

    let fixture = unsafe { Fixture::new(sq_test_compact_symbols(400), sq_test_symbols_delete) };
    let language = &fixture.grammar;
    let display = language.squatter_kind_id(KindId::from_raw(1)).unwrap();
    let original = language
        .squatter_grammar_id(GrammarId::from_raw(2))
        .unwrap();
    assert_eq!(language.squatter_kind_count(), 5);
    assert!(language.squatter_grammar_count() > 256);
    assert_eq!(id_width_flags([language]), 0);
    assert_eq!(language.squatter_kind_id(KindId::from_raw(2)), None);
    assert_eq!(language.squatter_kind_id(KindId::from_raw(4)), None);
    assert_eq!(language.squatter_grammar_id(GrammarId::from_raw(3)), None);
    assert_eq!(language.squatter_kind_id(KindId::from_raw(400)), None);
    assert_eq!(language.squatter_grammar_id(GrammarId::from_raw(400)), None);
    for id in [0, 5, u16::MAX] {
        assert_eq!(language.kind_id(SquatterKindId::from_raw(id)), None);
    }
    for id in [0, language.squatter_grammar_count() as u16, u16::MAX] {
        assert_eq!(language.grammar_id(SquatterGrammarId::from_raw(id)), None);
    }
    for native in [u16::MAX, u16::MAX - 1] {
        let kind = KindId::from_raw(native);
        let grammar = GrammarId::from_raw(native);
        assert_eq!(
            language.kind_id(language.squatter_kind_id(kind).unwrap()),
            Some(kind)
        );
        assert_eq!(
            language.grammar_id(language.squatter_grammar_id(grammar).unwrap()),
            Some(grammar)
        );
    }

    let mut builder = Builder::new(language, 1, false).unwrap();
    for index in 0..100 {
        builder
            .emit(
                &leaf(display.raw(), original.raw(), 0, u8::from(index == 0)),
                builder.distance(),
            )
            .unwrap();
    }
    let default = language
        .squatter_grammar_id(GrammarId::from_raw(1))
        .unwrap();
    builder
        .emit(&leaf(display.raw(), default.raw(), 0, 1), 0)
        .unwrap();
    let tree = builder
        .finish(PackOptions {
            repack: true,
            symbol_presence: &|_| true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(tree.data().layout.symbol_width, 2);
    let restored =
        Language::from_cache(&language.tree_sitter_language(), &language.cache().unwrap()).unwrap();
    let loaded = Forest::from_bytes(std::slice::from_ref(&restored), tree.as_bytes()).unwrap();
    assert_eq!(loaded.as_bytes(), tree.as_bytes());
    for node in loaded.root_node().preorder().nodes().skip(1) {
        assert_eq!(node.kind_id(), KindId::from_raw(1));
        assert_eq!(node.grammar_id(), GrammarId::from_raw(2));
        assert_eq!(node.squatter_kind_id(), display);
        assert_eq!(node.squatter_grammar_id(), original);
    }
    assert_eq!(tree.presence_cache().unwrap().as_bytes().len(), 16 + 5 * 8);
    assert_eq!(
        loaded
            .root_node()
            .all()
            .filter_squatter_kind_ids([display])
            .count(),
        101
    );
    assert_eq!(
        loaded
            .root_node()
            .all()
            .filter_squatter_kind_ids([SquatterKindId::from_raw(257)])
            .count(),
        0
    );

    let mut builder = Builder::new(language, 1, false).unwrap();
    let display = language.squatter_kind_id(KindId::from_raw(5)).unwrap();
    let default = language
        .squatter_grammar_id(GrammarId::from_raw(5))
        .unwrap();
    assert_ne!(display.raw(), default.raw());
    builder
        .emit(&leaf(display.raw(), default.raw(), 0, 1), 0)
        .unwrap();
    let tree = builder.finish(PackOptions::default()).unwrap();
    assert_eq!(tree.data().flags() & SEPARATE_GRAMMAR, SEPARATE_GRAMMAR);
    assert_eq!(tree.root_node().grammar_id(), GrammarId::from_raw(5));
    assert!(Forest::from_bytes(std::slice::from_ref(language), tree.as_bytes()).is_ok());
}

#[test]
fn matching_ids_omit_grammar_before_flag_columns() {
    for count in [16, 300] {
        let fixture = Fixture::symbols(count);
        for flags in [0, 2, 8, 2 | 8, 4 | 8, 2 | 4 | 8] {
            for repack in [false, true] {
                let mut builder = Builder::new(&fixture.grammar, 4, false).unwrap();
                for index in 0..2 * GROUP_SIZE {
                    builder
                        .emit(
                            &leaf(2, 2, 0, u8::from(index == 0) | flags),
                            builder.distance(),
                        )
                        .unwrap();
                }
                builder.emit(&leaf(2, 2, 0, 1), 0).unwrap();
                let tree = builder
                    .finish(PackOptions {
                        repack,
                        ..Default::default()
                    })
                    .unwrap();
                assert_eq!(tree.data().flags() & SEPARATE_GRAMMAR, 0);
                assert_eq!(tree.data().layout.grammar.0, tree.data().layout.extra.0);
                let tree =
                    Forest::from_bytes(std::slice::from_ref(&fixture.grammar), tree.as_bytes())
                        .unwrap();
                for node in tree.root_node().preorder().nodes().skip(1) {
                    assert_eq!(node.kind_id().raw(), 2);
                    assert_eq!(node.grammar_id().raw(), 2);
                    assert_eq!(node.is_extra(), flags & 2 != 0);
                    assert_eq!(node.is_missing(), flags & 4 != 0);
                    assert_eq!(node.has_error(), flags & 8 != 0);
                }
            }
        }
    }
}

#[test]
fn synthetic_symbol_ids_and_optional_columns() {
    for count in [16, 254, 255, 300, 32766, 32767] {
        let fixture = Fixture::symbols(count);
        let grammar = &fixture.grammar;
        for flags in [0, 2, 8, 2 | 8, 4 | 8, 2 | 4 | 8] {
            for points in [false, true] {
                let mut builder = Builder::new(grammar, 1, points).unwrap();
                for index in 0..65 * GROUP_SIZE {
                    let original = (index % 4 + 1) as u16;
                    builder
                        .emit(
                            &leaf(
                                if original <= 2 { 1 } else { original },
                                original,
                                0,
                                u8::from(index == 0)
                                    | if index == 2 || index == 63 * GROUP_SIZE {
                                        flags
                                    } else {
                                        0
                                    },
                            ),
                            builder.distance(),
                        )
                        .unwrap();
                }
                builder.emit(&leaf(1, 1, 0, 1), 0).unwrap();
                let mut tree = builder
                    .finish(PackOptions {
                        points,
                        ..Default::default()
                    })
                    .unwrap();
                if count == 16 && flags == 0 && !points {
                    let data = tree.data();
                    assert_ne!(data.flags() & SEPARATE_GRAMMAR, 0);
                    let symbols = data
                        .layout
                        .symbol
                        .offset(std::ptr::NonNull::from(data.slice()).cast());
                    let originals = data
                        .layout
                        .grammar
                        .offset(std::ptr::NonNull::from(data.slice()).cast());
                    assert_eq!(
                        originals,
                        data.layout
                            .last
                            .offset(std::ptr::NonNull::from(data.slice()).cast())
                            + bit_bytes(tree.group_capacity() * GROUP_SIZE) as usize
                    );
                    for slot in 0..3 * GROUP_SIZE {
                        let original = (slot % 4 + 1) as u16;
                        assert_eq!(
                            data.symbol_index(slot).raw(),
                            if original <= 2 { 1 } else { original }
                        );
                        assert_eq!(data.grammar_index(slot).raw(), original);
                    }
                    assert_eq!(
                        Layout::new(4, FOREST_FORMAT | BYTE_IDS | SEPARATE_GRAMMAR)
                            .unwrap()
                            .end
                            .raw(),
                        Layout::new(4, FOREST_FORMAT | BYTE_IDS).unwrap().end.raw()
                            + 4 * GROUP_SIZE,
                    );
                    let mut bytes = tree.as_bytes().to_vec();
                    bytes[..4].copy_from_slice(&(data.flags() & !SEPARATE_GRAMMAR).to_le_bytes());
                    assert!(Forest::from_bytes(std::slice::from_ref(grammar), &bytes).is_err());
                    assert!(
                        Forest::from_bytes_safety_checked(std::slice::from_ref(grammar), &bytes)
                            .is_err()
                    );
                    for column in [symbols, originals] {
                        let mut bytes = tree.as_bytes().to_vec();
                        bytes[column] = u8::MAX;
                        assert!(Forest::from_bytes(std::slice::from_ref(grammar), &bytes).is_err());
                    }
                }
                for pass in 0..3 {
                    for node in tree.root_node().preorder().nodes().skip(1) {
                        let slot = node.slot().raw();
                        let original = (slot % 4 + 1) as u16;
                        assert_eq!(
                            node.kind_id().raw(),
                            if original <= 2 { 1 } else { original }
                        );
                        assert_eq!(node.grammar_id().raw(), original);
                        assert_eq!(
                            node.is_extra(),
                            flags & 2 != 0 && [2, 63 * GROUP_SIZE].contains(&slot)
                        );
                        assert_eq!(
                            node.is_missing(),
                            flags & 4 != 0 && [2, 63 * GROUP_SIZE].contains(&slot)
                        );
                        assert_eq!(
                            node.has_error(),
                            flags & 8 != 0 && [2, 63 * GROUP_SIZE].contains(&slot)
                        );
                    }
                    let compact = tree.repack().unwrap();
                    let copy =
                        Forest::from_bytes(std::slice::from_ref(grammar), compact.as_bytes())
                            .unwrap();
                    let borrowed = Forest::from_bytes_borrowed(
                        std::slice::from_ref(grammar),
                        compact.as_bytes(),
                    )
                    .unwrap();
                    assert_eq!(copy.as_bytes(), borrowed.as_bytes());
                    assert_eq!(compact.group_capacity(), compact.group_count());
                    tree = copy;
                    tree.resize(
                        tree.group_count() + if pass == 0 { 17 } else { 0 },
                        tree.data().flags(),
                    )
                    .unwrap();
                }
                let mut invalid = tree.as_bytes().to_vec();
                let offset = if count == 32767 {
                    tree.data().layout.grammar
                } else {
                    tree.data().layout.symbol
                };
                let offset = offset.offset(std::ptr::NonNull::from(tree.data().slice()).cast());
                if tree.data().layout.symbol_width == 1 {
                    // Zero is reserved and cannot be stored on a node.
                    invalid[offset] = 0;
                } else {
                    invalid[offset..offset + 2].copy_from_slice(&u16::MAX.to_le_bytes());
                }
                assert!(Forest::from_bytes(std::slice::from_ref(grammar), &invalid).is_err());
                assert!(
                    Forest::from_bytes_safety_checked(std::slice::from_ref(grammar), &invalid)
                        .is_err()
                );
            }
        }
    }
}

#[test]
fn maximum_spans_roundtrip_and_reject_delta_underflow() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();

    let source = format!("[{}0]", "0,".repeat(33000));
    let tree = Forest::parse(&grammar, &mut parser, &source).unwrap();
    assert!(tree.root_node().slot().raw() > u16::MAX as u32);
    let loaded = Forest::from_bytes(std::slice::from_ref(&grammar), tree.as_bytes()).unwrap();
    let array = loaded
        .root_node()
        .named_child(crate::NamedChildIx::new(0))
        .unwrap();
    assert_eq!(array.named_child_count().raw(), 33001);
    for index in [0, 33000] {
        let child = array.named_child(crate::NamedChildIx::new(index)).unwrap();
        assert_eq!(child.parent(), Some(array));
        assert_eq!(child.start_byte(), 1 + index as usize * 2);
    }

    let mut small = Forest::parse(&grammar, &mut parser, "0").unwrap();
    let data = small.data_mut();
    let maximum = data.word(data.layout.span_max, 0);
    put_span_delta(data, 0, (maximum + 1) as u16);
    assert!(Forest::from_bytes(std::slice::from_ref(&grammar), small.as_bytes()).is_err());
    assert!(
        Forest::from_bytes_safety_checked(std::slice::from_ref(&grammar), small.as_bytes())
            .is_err()
    );
}

#[test]
fn navigation_across_every_waste_boundary() {
    let fixture = Fixture::symbols(16);
    let mut tree = synthetic_forest(&fixture.grammar, 3).unwrap();
    tree.data_mut().put_word(SlabOffset(0), 1, 3);
    for first in 0..GROUP_SIZE {
        for second in 0..GROUP_SIZE {
            let mut slots = Vec::new();
            let waste = [first, second, (first + second) % GROUP_SIZE];
            let data = tree.data_mut();
            for group in 0..3 {
                data.put_short(data.layout.waste, group as u32, waste[group] as u16);
                data.put_word(data.layout.span_max, group as u32, 3 * GROUP_SIZE);
                for lane in 0..GROUP_SIZE - waste[group] {
                    let slot = group as u32 * GROUP_SIZE + lane;
                    slots.push(SlotIx::from_raw(slot));
                    put_span_delta(
                        data,
                        slot,
                        (3 * GROUP_SIZE) as u16
                            - if group > 0 && lane == 0 {
                                waste[group - 1] as u16
                            } else {
                                0
                            },
                    );
                    data.put_bit(data.layout.last, slot, slot == 0);
                }
            }
            let root = *slots.last().unwrap();
            put_span_delta(data, root.raw(), (3 * GROUP_SIZE - root.raw()) as u16);
            data.put_bit(data.layout.last, root.raw(), true);
            assert_eq!(tree.root_node().slot(), root);
            let expected: Vec<_> = slots.iter().rev().copied().collect();
            assert_eq!(
                tree.root_node()
                    .preorder()
                    .nodes()
                    .map(|node| node.slot())
                    .collect::<Vec<_>>(),
                expected
            );
            for (index, &slot) in slots.iter().enumerate() {
                let node = tree.node_at_slot(slot).unwrap();
                assert_eq!(
                    node.next_preorder().map(|node| node.slot()),
                    index.checked_sub(1).map(|index| slots[index])
                );
                assert_eq!(
                    node.prev_preorder().map(|node| node.slot()),
                    slots.get(index + 1).copied()
                );
            }
            for group in 0..3 {
                for lane in GROUP_SIZE - waste[group]..GROUP_SIZE {
                    assert!(
                        tree.node_at_slot(SlotIx::from_raw(group as u32 * GROUP_SIZE + lane))
                            .is_none()
                    );
                }
            }
            assert!(
                tree.node_at_slot(SlotIx::from_raw(3 * GROUP_SIZE))
                    .is_none()
            );
            let mut cursor = tree.root_node().walk();
            assert!(cursor.goto_first_child());
            for slot in expected.iter().skip(1) {
                assert_eq!(cursor.node().slot(), *slot);
                assert_eq!(cursor.depth(), 1);
                assert_eq!(cursor.goto_next_sibling(), *slot != slots[0]);
            }
            assert!(cursor.goto_parent());
            assert_eq!(cursor.node().slot(), root);
        }
    }
}

fn exercise_columns(tree: &mut Forest, fill: bool) {
    let capacity = tree.group_capacity();
    let data = tree.data_mut();
    let layout = data.layout;
    for (tag, (offset, bits, scale)) in [
        (layout.waste, 16, 1),
        (layout.span_max, 32, 1),
        (layout.start_byte_base, 32, 1),
        (layout.end_byte_base, 32, 1),
        (layout.last, 1, GROUP_SIZE),
        (layout.extra, 1, GROUP_SIZE),
        (layout.error, 1, GROUP_SIZE),
        (layout.missing, 1, GROUP_SIZE),
        (layout.span_delta, SPAN_BITS as usize, GROUP_SIZE),
        (layout.start_byte_delta, 8, GROUP_SIZE),
        (layout.end_byte_delta, 16, GROUP_SIZE),
        (layout.supertype, 16, GROUP_SIZE),
        (layout.symbol, layout.symbol_width as usize * 8, GROUP_SIZE),
        (layout.field, 16, GROUP_SIZE),
        (layout.grammar, layout.symbol_width as usize * 8, GROUP_SIZE),
    ]
    .into_iter()
    .enumerate()
    {
        for index in 0..capacity * scale {
            let expected = if index < 2 * scale {
                (index as u64 * 0x123456789abcdef + tag as u64) & (u64::MAX >> (64 - bits))
            } else {
                0
            };
            if fill {
                match bits {
                    1 => data.put_bit(offset, index, expected != 0),
                    8 => data.put_byte(offset, index, expected as u8),
                    16 => data.put_short(offset, index, expected as u16),
                    32 => data.put_word(offset, index, expected as u32),
                    64 => data.put_long(offset, index, expected),
                    _ => unreachable!(),
                }
            }
            let actual = match bits {
                1 => u64::from(data.bit(offset, index)),
                8 => data.byte(offset, index) as u64,
                16 => data.short(offset, index) as u64,
                32 => data.word(offset, index) as u64,
                64 => data.long(offset, index),
                _ => unreachable!(),
            };
            assert_eq!(actual, expected, "column {tag}, index {index}");
            if bits > 1 {
                let start = offset.offset(std::ptr::NonNull::from(data.slice()).cast())
                    + index as usize * (bits / 8);
                assert_eq!(
                    &data.slice()[start..start + bits / 8],
                    &expected.to_le_bytes()[..bits / 8]
                );
            }
        }
    }
}

#[test]
fn column_growth_compaction_and_little_endian_encoding() {
    for count in [16, 32767] {
        let fixture = Fixture::symbols(count);
        let mut tree = synthetic_forest(&fixture.grammar, 3).unwrap();
        assert_eq!(
            tree.data().flags(),
            FOREST_FORMAT | OPTIONAL | id_width_flags([&fixture.grammar])
        );
        tree.data_mut().put_word(SlabOffset(0), 1, 2);
        exercise_columns(&mut tree, true);
        for capacity in [7, 19, 2, 31, 2] {
            if capacity < tree.group_capacity() {
                tree.finish_layout(capacity, tree.data().flags() & OPTIONAL)
                    .unwrap();
            } else {
                tree.resize(capacity, tree.data().flags()).unwrap();
            }
            exercise_columns(&mut tree, false);
        }
        assert_eq!(
            tree.resize(u32::MAX, tree.data().flags()),
            Err(Error::Overflow)
        );
        exercise_columns(&mut tree, false);
    }
}

#[test]
fn invalid_waste_and_absent_fields() {
    let fixture = Fixture::supertypes(0, false);
    for count in 1..=GROUP_SIZE {
        let mut builder = Builder::new(&fixture.grammar, 1, false).unwrap();
        for index in 0..count - 1 {
            builder
                .emit(&leaf(1, 1, 0, u8::from(index == 0)), builder.distance())
                .unwrap();
        }
        builder.emit(&leaf(1, 1, 0, 1), 0).unwrap();
        let tree = builder.finish(PackOptions::default()).unwrap();
        assert_eq!(
            tree.root_node().all().filter_field_ids([None]).count(),
            count as usize
        );
        assert_eq!(
            tree.root_node()
                .all()
                .filter_field_ids([FieldId::from_raw(1)])
                .count(),
            0
        );
        for waste in [GROUP_SIZE, GROUP_SIZE + 1, u16::MAX as u32] {
            let mut bytes = tree.as_bytes().to_vec();
            let start = tree
                .data()
                .layout
                .waste
                .offset(std::ptr::NonNull::from(tree.data().slice()).cast());
            bytes[start..start + 2].copy_from_slice(&(waste as u16).to_le_bytes());
            assert!(Forest::from_bytes(std::slice::from_ref(&fixture.grammar), &bytes).is_err());
            assert!(
                Forest::from_bytes_borrowed(std::slice::from_ref(&fixture.grammar), &bytes)
                    .is_err()
            );
            assert!(
                Forest::from_bytes_safety_checked(std::slice::from_ref(&fixture.grammar), &bytes)
                    .is_err()
            );
        }
    }
}

#[test]
fn packing_rejects_wrong_grammar_and_recovers_after_overflow() {
    unsafe extern "C" {
        fn sq_test_clone_language(language: *const c_void) -> *const c_void;
    }
    let pointer = unsafe { sq_test_parser_language() };
    let language = unsafe { tree_sitter::Language::from_raw(pointer.cast()) };
    let grammar = Language::new(&language).unwrap();
    let other = unsafe { Fixture::new(sq_test_clone_language(pointer), sq_test_supertypes_delete) };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse("\nx", None).unwrap();
    let mut context = Packer::new().unwrap();
    assert_eq!(
        context.pack(&other.grammar, &native).unwrap_err(),
        Error::Language
    );
    let retained = context.pack(&grammar, &native).unwrap();
    let bytes = retained.as_bytes().to_vec();
    assert_eq!(
        context
            .pack_with_options(
                &grammar,
                &native,
                PackOptions {
                    initial_group_capacity: u32::MAX,
                    ..Default::default()
                }
            )
            .unwrap_err(),
        Error::Overflow
    );
    assert_eq!(context.pack(&grammar, &native).unwrap().as_bytes(), bytes);
    context.drop_scratch();
    assert_eq!(context.pack(&grammar, &native).unwrap().as_bytes(), bytes);
    drop(context);
    drop(grammar);
    assert_eq!(retained.as_bytes(), bytes);
}
