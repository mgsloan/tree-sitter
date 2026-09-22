use super::*;
use crate::{GrammarKindId, Parser, SlotIx};
use std::ffi::c_void;

unsafe extern "C" {
    fn sq_test_dictionaries();
    fn sq_test_grammar_limits();
    fn sq_test_terminal_aliases();
    fn sq_test_lexer_fallback();
    fn sq_test_unsupported_parsers();
    fn sq_test_parser_language() -> *const c_void;
    fn sq_test_supertypes(count: u32, connected: bool) -> *const c_void;
    fn sq_test_supertypes_delete(language: *const c_void);
    fn sq_test_symbols(count: u32) -> *const c_void;
    fn sq_test_symbols_delete(language: *const c_void);
}

struct LanguageOwner(*const c_void, unsafe extern "C" fn(*const c_void));

impl Drop for LanguageOwner {
    fn drop(&mut self) {
        unsafe { (self.1)(self.0) };
    }
}

struct Fixture {
    grammar: Grammar,
    // The synthetic language must outlive every prepared grammar and tree.
    _owner: LanguageOwner,
}

impl Fixture {
    unsafe fn new(pointer: *const c_void, delete: unsafe extern "C" fn(*const c_void)) -> Self {
        let language = unsafe { tree_sitter::Language::from_raw(pointer.cast()) };
        Self {
            grammar: Grammar::new(&language).unwrap(),
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
        sq_test_terminal_aliases();
        sq_test_grammar_limits();
        sq_test_unsupported_parsers();
    }
}

#[test]
fn lexer_fallback_and_concurrent_parser_preparation() {
    unsafe { sq_test_lexer_fallback() };
    let language = unsafe { tree_sitter::Language::from_raw(sq_test_parser_language().cast()) };
    let grammar = Grammar::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse("\nx", None).unwrap();
    let options = PackOptions {
        initial_group_capacity: 1,
        repack: true,
        ..Default::default()
    };
    let expected = Tree::pack_with_options(&grammar, &native, options).unwrap();
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let grammar = grammar.clone();
                barrier.wait();
                for _ in 0..4 {
                    let mut parser = Parser::new(&grammar).unwrap();
                    assert!(parser.parse("?").is_err());
                    assert_eq!(
                        parser
                            .parse_with_options("\nx", options)
                            .unwrap()
                            .as_bytes(),
                        expected.as_bytes()
                    );
                    parser.trim();
                    assert_eq!(
                        parser
                            .parse_with_options("\nx", options)
                            .unwrap()
                            .as_bytes(),
                        expected.as_bytes()
                    );
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
        symbol: RemappedKindId(symbol),
        grammar: RemappedGrammarKindId(grammar),
        field: None,
        supertype,
        flags: flags.into(),
    }
}

fn check_masks(tree: &Tree, slots: &[SlotIx], bits: u32) {
    for (mask, &slot) in slots.iter().enumerate() {
        let node = tree.node_at_slot(slot).unwrap();
        for bit in 0..bits {
            assert_eq!(
                node.has_supertype(GrammarKindId::new((bit + 2) as u16)),
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
        for count in [1 << bits, (1 << bits).min(257)] {
            let mut builder = Builder::new(grammar, 1, true).unwrap();
            let mut slots = Vec::new();
            for mask in 0..count {
                builder
                    .emit(
                        &leaf(1, 1, mask as u16, u8::from(mask == 0)),
                        builder.distance(),
                    )
                    .unwrap();
                slots.push(SlotIx::new(builder.distance() - 1));
            }
            builder.emit(&leaf(1, 1, 0, 1), 0).unwrap();
            let mut tree = builder
                .finish(&mut PresenceScratch::default(), PackOptions::default())
                .unwrap();
            let groups = tree.group_count();
            for capacity in [groups + 17, groups, groups + 1] {
                let trailing = tree.as_bytes().len() as u32 - tree.data().layout.end.get();
                tree.resize(capacity, tree.data().flags(), trailing, true)
                    .unwrap();
                check_masks(&tree, &slots, bits);
                let loaded = Tree::from_bytes(grammar, tree.as_bytes()).unwrap();
                let borrowed = Tree::from_bytes_borrowed(grammar, tree.as_bytes()).unwrap();
                check_masks(&loaded, &slots, bits);
                check_masks(&borrowed, &slots, bits);
                let restored =
                    Grammar::from_cache(&grammar.language(), &grammar.cache().unwrap()).unwrap();
                check_masks(
                    &Tree::from_bytes(&restored, tree.as_bytes()).unwrap(),
                    &slots,
                    bits,
                );
            }
            for (offset, mask) in [(0, 1), (1, 4), (14, 1)] {
                let mut bytes = tree.as_bytes().to_vec();
                bytes[offset] ^= mask;
                assert!(Tree::from_bytes(grammar, &bytes).is_err());
                assert!(Tree::from_bytes_safety_checked(grammar, &bytes).is_err());
            }
        }
    }
}

#[test]
fn synthetic_symbol_encodings_and_optional_columns() {
    for count in [16, 300, 32766, 32767] {
        let fixture = Fixture::symbols(count);
        let grammar = &fixture.grammar;
        assert_eq!(grammar.tables().separate != 0, count == 32767);
        for flags in [0, 2, 8, 2 | 8, 4 | 8, 2 | 4 | 8] {
            for points in [false, true] {
                let mut builder = Builder::new(grammar, 1, points).unwrap();
                for index in 0..65 * GROUP_SIZE {
                    let original = (index % 4) as u16;
                    builder
                        .emit(
                            &leaf(
                                if original < 2 { 0 } else { original },
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
                builder.emit(&leaf(0, 0, 0, 1), 0).unwrap();
                let mut tree = builder
                    .finish(
                        &mut PresenceScratch::default(),
                        PackOptions {
                            points,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                for pass in 0..3 {
                    for node in tree.root_node().preorder().nodes().skip(1) {
                        let slot = node.slot().get();
                        let original = (slot % 4) as u16;
                        assert_eq!(
                            node.kind_id().get(),
                            if original < 2 { 0 } else { original }
                        );
                        assert_eq!(node.grammar_id().get(), original);
                        assert_eq!(
                            node.is_extra(),
                            flags & 2 != 0 && [2, 63 * GROUP_SIZE].contains(&slot)
                        );
                        assert_eq!(
                            node.is_missing(),
                            flags & 4 != 0 && [2, 63 * GROUP_SIZE].contains(&slot)
                        );
                    }
                    let compact = tree.repack().unwrap();
                    let copy = Tree::from_bytes(grammar, compact.as_bytes()).unwrap();
                    let borrowed = Tree::from_bytes_borrowed(grammar, compact.as_bytes()).unwrap();
                    assert_eq!(copy.as_bytes(), borrowed.as_bytes());
                    assert_eq!(compact.group_capacity(), compact.group_count());
                    tree = copy;
                    let trailing = tree.as_bytes().len() as u32 - tree.data().layout.end.get();
                    tree.resize(
                        tree.group_count() + if pass == 0 { 17 } else { 0 },
                        tree.data().flags(),
                        trailing,
                        true,
                    )
                    .unwrap();
                }
                let mut invalid = tree.as_bytes().to_vec();
                let offset = if count == 32767 {
                    tree.data().layout.grammar
                } else {
                    tree.data().layout.symbol
                };
                invalid[offset.get() as usize..offset.get() as usize + 2]
                    .copy_from_slice(&u16::MAX.to_le_bytes());
                assert!(Tree::from_bytes(grammar, &invalid).is_err());
                assert!(Tree::from_bytes_safety_checked(grammar, &invalid).is_err());
            }
        }
    }
}

#[test]
fn navigation_across_every_waste_boundary() {
    let fixture = Fixture::symbols(16);
    let mut tree = Tree::empty(&fixture.grammar, 3, false).unwrap();
    tree.data_mut().put_word(SlabOffset(0), 1, 3);
    for first in 0..GROUP_SIZE {
        for second in 0..GROUP_SIZE {
            let mut slots = Vec::new();
            let waste = [first, second, (first + second) % GROUP_SIZE];
            let data = tree.data_mut();
            for group in 0..3 {
                data.put_short(data.layout.waste, group as u32, waste[group] as u16);
                for lane in 0..GROUP_SIZE - waste[group] {
                    let slot = group as u32 * GROUP_SIZE + lane;
                    slots.push(SlotIx::new(slot));
                    data.put_byte(
                        data.layout.span_delta,
                        slot,
                        if group > 0 && lane == 0 {
                            waste[group - 1] as u8
                        } else {
                            0
                        },
                    );
                    data.put_bit(data.layout.last, slot, slot == 0);
                }
            }
            let root = *slots.last().unwrap();
            data.put_byte(data.layout.span_delta, root.get(), root.get() as u8);
            data.put_bit(data.layout.last, root.get(), true);
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
                        tree.node_at_slot(SlotIx::new(group as u32 * GROUP_SIZE + lane))
                            .is_none()
                    );
                }
            }
            assert!(tree.node_at_slot(SlotIx::new(3 * GROUP_SIZE)).is_none());
            let mut cursor = tree.root_node().walk().unwrap();
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

fn exercise_columns(tree: &mut Tree, fill: bool) {
    let capacity = tree.group_capacity();
    let data = tree.data_mut();
    let layout = data.layout;
    for (tag, (offset, bits, scale)) in [
        (layout.waste, 16, 1),
        (layout.span_base, 32, 1),
        (layout.start_byte_base, 32, 1),
        (layout.end_byte_base, 32, 1),
        (layout.start_point_base, 64, 1),
        (layout.end_point_base, 64, 1),
        (layout.last, 1, GROUP_SIZE),
        (layout.extra, 1, GROUP_SIZE),
        (layout.error, 1, 1),
        (layout.missing, 1, GROUP_SIZE),
        (layout.span_delta, 8, GROUP_SIZE),
        (layout.start_byte_delta, 8, GROUP_SIZE),
        (layout.end_byte_delta, 16, GROUP_SIZE),
        (layout.start_point, 16, GROUP_SIZE),
        (layout.end_point, 16, GROUP_SIZE),
        (layout.supertype, 16, GROUP_SIZE),
        (layout.symbol, 16, GROUP_SIZE),
        (layout.field, 16, GROUP_SIZE),
        (layout.grammar, 16, GROUP_SIZE),
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
                let start = offset.get() as usize + index as usize * (bits / 8);
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
    let fixture = Fixture::symbols(32767);
    let mut tree = Tree::empty(&fixture.grammar, 3, true).unwrap();
    tree.data_mut().put_word(SlabOffset(0), 1, 2);
    exercise_columns(&mut tree, true);
    for capacity in [7, 19, 2, 31, 2] {
        tree.resize(capacity, tree.data().flags(), 0, false)
            .unwrap();
        exercise_columns(&mut tree, false);
    }
    assert_eq!(
        tree.resize(u32::MAX, tree.data().flags(), 0, false),
        Err(Error::Overflow)
    );
    exercise_columns(&mut tree, false);
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
        let tree = builder
            .finish(&mut PresenceScratch::default(), PackOptions::default())
            .unwrap();
        assert_eq!(
            tree.root_node().all().filter_field_ids([None]).count(),
            count as usize
        );
        assert_eq!(
            tree.root_node()
                .all()
                .filter_field_ids([FieldId::new(1)])
                .count(),
            0
        );
        for waste in [GROUP_SIZE, GROUP_SIZE + 1, u16::MAX as u32] {
            let mut bytes = tree.as_bytes().to_vec();
            let start = tree.data().layout.waste.get() as usize;
            bytes[start..start + 2].copy_from_slice(&(waste as u16).to_le_bytes());
            assert!(Tree::from_bytes(&fixture.grammar, &bytes).is_err());
            assert!(Tree::from_bytes_borrowed(&fixture.grammar, &bytes).is_err());
            assert!(Tree::from_bytes_safety_checked(&fixture.grammar, &bytes).is_err());
        }
    }
}

#[test]
fn presence_ignores_waste_and_invalid_symbols() {
    for count in [16, 300, 32766, 32767] {
        let fixture = Fixture::symbols(count);
        let grammar = &fixture.grammar;
        let mut tree = Tree::empty(grammar, 33, false).unwrap();
        tree.data_mut().put_word(SlabOffset(0), 1, 33);
        for group in 0..33 {
            let data = tree.data_mut();
            data.put_short(data.layout.waste, group, (GROUP_SIZE - 3) as u16);
            for lane in 0..GROUP_SIZE {
                let original = if lane >= 3 {
                    2
                } else if lane == 0 {
                    1
                } else if lane == 1 {
                    count + group % 2
                } else if group == 0 {
                    count - 1
                } else {
                    1
                };
                let code = unsafe { *grammar.tables().default_codes.add(original as usize) };
                data.put_short(data.layout.symbol, group * GROUP_SIZE + lane, code);
            }
        }
        for indexed in [false, true] {
            if indexed {
                let trailing = presence_size(count + 2, 33) as u32;
                tree.resize(33, tree.data().flags(), trailing, false)
                    .unwrap();
                tree.build_presence(&mut PresenceScratch::default());
            }
            for symbol in [
                0,
                1,
                2,
                count - 1,
                count,
                count + 1,
                count + 2,
                65533,
                65534,
                65535,
            ] {
                let symbol = crate::KindId::new(symbol as u16);
                for group in 0..33 {
                    let expected = symbol.get() == 0
                        || (group == 0 && u32::from(symbol.get()) == count - 1)
                        || symbol.get() == if group % 2 == 0 { 65535 } else { 65534 };
                    assert_eq!(
                        tree.group_has_symbol(group, symbol),
                        expected,
                        "count={count}, symbol={symbol:?}, group={group}, indexed={indexed}"
                    );
                }
                assert!(!tree.group_has_symbol(33, symbol));
                assert!(!tree.group_has_symbol(u32::MAX, symbol));
            }
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
    let grammar = Grammar::new(&language).unwrap();
    let other = unsafe { Fixture::new(sq_test_clone_language(pointer), sq_test_supertypes_delete) };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse("\nx", None).unwrap();
    let mut context = PackContext::new().unwrap();
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
    context.trim();
    assert_eq!(context.pack(&grammar, &native).unwrap().as_bytes(), bytes);
    drop(context);
    drop(grammar);
    assert_eq!(retained.as_bytes(), bytes);
}
