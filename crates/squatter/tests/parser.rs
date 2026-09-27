mod support;

use std::ops::ControlFlow;
use support::{c_language, json_language};
use tree_squatter::{
    Error, Language, PackOptions, PackedParseOptions, ParseOptions, Parser, ParserError, Tree,
    TreeFellerParser,
    traits::{NodeLike, Parse, ParseStateLike, ParseWithCallback, TreeLike},
};

fn compatible(language: &Language) -> Parser {
    let mut parser = Parser::new();
    parser.set_language(language).unwrap();
    parser
}

fn check_generic<P: Parse>(parser: &mut P)
where
    P::Error: std::fmt::Debug,
{
    let source = "int value;";
    let tree = Parse::parse(parser, source).unwrap();
    assert_eq!(tree.root_node().byte_range(), 0..source.len());
    let mut progress = |state: &dyn ParseStateLike| {
        assert_eq!(state.is_converting(), state.current_byte_offset_descends());
        assert!(!state.has_error());
        ControlFlow::Continue(())
    };
    let tree = parser
        .parse_with_options(
            source,
            ParseOptions::new().progress_callback(&mut progress).into(),
        )
        .unwrap();
    assert_eq!(tree.root_node().kind(), "translation_unit");
}

#[test]
fn shared_traits_and_no_language() {
    let mut parser = Parser::new();
    assert!(parser.language().is_none());
    assert_eq!(parser.parse("x").unwrap_err(), ParserError::NoLanguage);
    assert_eq!(
        parser
            .parse_with_callback(&mut |_, _| b"" as &[u8], Default::default())
            .unwrap_err(),
        ParserError::NoLanguage
    );
    let mut native = tree_sitter::Parser::new();
    assert_eq!(
        Parse::parse(&mut native, "x").unwrap_err(),
        ParserError::NoLanguage
    );
    assert_eq!(
        ParseWithCallback::parse_with_callback(
            &mut native,
            &mut |_, _| b"" as &[u8],
            Default::default()
        )
        .unwrap_err(),
        ParserError::NoLanguage
    );

    let language = Language::new(&c_language()).unwrap();
    parser.set_language(&language).unwrap();
    native.set_language(&c_language()).unwrap();
    let direct = TreeFellerParser::new(&language).unwrap();
    assert_eq!(direct.language().tree_sitter_language(), c_language());
    check_generic(&mut parser);
    check_generic(&mut native);
}

fn callback_tree<P: ParseWithCallback>(parser: &mut P, source: &[u8]) -> P::Tree
where
    P::Error: std::fmt::Debug,
{
    let mut reads = 0;
    let tree = parser
        .parse_with_callback(
            &mut |byte, point| {
                reads += 1;
                assert!(byte <= source.len());
                let prefix = &source[..byte];
                let row = prefix.iter().filter(|&&value| value == b'\n').count();
                let column = prefix
                    .iter()
                    .rposition(|&value| value == b'\n')
                    .map_or(byte, |newline| byte - newline - 1);
                assert_eq!(point, tree_sitter::Point::new(row, column));
                source[byte..(byte + 3).min(source.len())].to_vec()
            },
            Default::default(),
        )
        .unwrap();
    assert!(reads > 2);
    tree
}

#[test]
fn callback_input_and_error_recovery() {
    let language = Language::new(&json_language()).unwrap();
    let mut parser = compatible(&language);
    let source = "{\n\"name\": \"😀π\",\n\"array\": [1, 2]}";
    let packed = callback_tree(&mut parser, source.as_bytes());
    let contiguous = parser.parse(source).unwrap();
    assert_eq!(packed.as_bytes(), contiguous.as_bytes());
    assert_eq!(
        packed.point_data().unwrap().as_bytes(),
        contiguous.point_data().unwrap().as_bytes()
    );
    let mut native = tree_sitter::Parser::new();
    native.set_language(&json_language()).unwrap();
    let tree = callback_tree(&mut native, source.as_bytes());
    assert_eq!(
        tree.root_node().byte_range(),
        packed.root_node().byte_range()
    );

    let mut saw_error = false;
    let mut progress = |state: &dyn ParseStateLike| {
        if state.is_converting() {
            assert!(state.has_error());
            saw_error = true;
        }
        ControlFlow::Continue(())
    };
    let recovered = parser
        .parse_with_options(
            "{broken",
            ParseOptions::new().progress_callback(&mut progress).into(),
        )
        .unwrap();
    assert!(recovered.root_node().has_error());
    assert!(saw_error);
    parser
        .set_language(&Language::new(&c_language()).unwrap())
        .unwrap();
    assert!(
        parser
            .parse("int broken = ;")
            .unwrap()
            .root_node()
            .has_error()
    );
    let mut direct = TreeFellerParser::new(parser.language().unwrap()).unwrap();
    assert_eq!(
        direct.parse("int broken = ;").unwrap_err().code,
        Error::Parse
    );
    parser.reset();
    parser.drop_scratch();
    assert_eq!(
        parser.language().unwrap().tree_sitter_language(),
        c_language()
    );
    assert!(!parser.parse("int good;").unwrap().root_node().has_error());
    drop(parser);
    assert_eq!(packed.root_node().byte_range(), 0..source.len());
}

fn check_packed<P>(parser: &mut P, source: &str) -> Tree
where
    P: Parse<Tree = Tree>,
    for<'a> P: Parse<Options<'a> = PackedParseOptions<'a>>,
    P::Error: std::fmt::Debug,
{
    let mut offsets = Vec::new();
    let mut parsing = false;
    let mut progress = |state: &dyn ParseStateLike| {
        assert_eq!(state.is_converting(), state.current_byte_offset_descends());
        assert!(!state.has_error());
        if state.is_converting() {
            offsets.push(state.current_byte_offset());
        } else {
            assert!(offsets.is_empty());
            parsing = true;
        }
        ControlFlow::Continue(())
    };
    let mut options = PackedParseOptions {
        parse: ParseOptions::new().progress_callback(&mut progress),
        pack: PackOptions {
            initial_group_capacity: 1,
            repack: true,
            ..Default::default()
        },
    };
    let tree = parser
        .parse_with_options(source, options.reborrow())
        .unwrap();
    assert!(parsing);
    assert_eq!(offsets[0], 0);
    assert!(offsets.windows(2).any(|pair| pair[0] > pair[1]));
    tree
}

#[test]
fn packed_options_progress_and_equivalence() {
    let language = Language::new(&c_language()).unwrap();
    let source = "int value = 123;\n".repeat(1000);
    let packed = check_packed(&mut compatible(&language), &source);
    let direct = TreeFellerParser::new(&language)
        .unwrap()
        .parse_with_options(
            &source,
            PackOptions {
                initial_group_capacity: 1,
                repack: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(packed.as_bytes(), direct.as_bytes());
    assert_eq!(
        packed.point_data().unwrap().as_bytes(),
        direct.point_data().unwrap().as_bytes()
    );
    assert_eq!(
        packed.presence_cache().unwrap().as_bytes(),
        direct.presence_cache().unwrap().as_bytes()
    );

    let mut parser = compatible(&language);
    let tree = parser
        .parse_with_options(
            &source,
            PackedParseOptions {
                pack: PackOptions {
                    points: false,
                    symbol_presence: false,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    assert!(tree.point_data().is_none());
    assert!(tree.presence_cache().is_none());
}

fn check_cancellation<P: Parse>(parser: &mut P, source: &str, converting: bool)
where
    P::Error: std::fmt::Debug,
{
    let mut reports = 0;
    let mut progress = |state: &dyn ParseStateLike| {
        if state.is_converting() == converting {
            reports += 1;
        }
        ControlFlow::Continue(())
    };
    parser
        .parse_with_options(
            source,
            ParseOptions::new().progress_callback(&mut progress).into(),
        )
        .unwrap();
    assert!(reports > 3);
    for stop_at in [1, reports / 2, reports - 1, reports] {
        let mut count = 0;
        let mut progress = |state: &dyn ParseStateLike| {
            if state.is_converting() == converting {
                count += 1;
                if count == stop_at {
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        };
        assert!(
            parser
                .parse_with_options(
                    source,
                    ParseOptions::new().progress_callback(&mut progress).into()
                )
                .is_err()
        );
        assert_eq!(count, stop_at);
        let tree = Parse::parse(parser, "int after;").unwrap();
        assert_eq!(tree.root_node().byte_range(), 0..10);
        assert!(!tree.root_node().has_error());
    }
}

#[test]
fn cancellation_in_both_phases_and_reuse() {
    let language = Language::new(&c_language()).unwrap();
    let source = "int value = 123;\n".repeat(1000);
    let mut parser = compatible(&language);
    for converting in [false, true] {
        check_cancellation(&mut parser, &source, converting);
    }
    let mut native = tree_sitter::Parser::new();
    native.set_language(&c_language()).unwrap();
    check_cancellation(&mut native, &source, false);
}

#[test]
fn packing_failure_and_reuse() {
    let language = Language::new(&c_language()).unwrap();
    let mut parser = compatible(&language);
    let options = PackedParseOptions {
        pack: PackOptions {
            initial_group_capacity: u32::MAX,
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        parser
            .parse_with_options("int value;", options)
            .unwrap_err(),
        ParserError::Pack(Error::Overflow)
    );
    assert!(!parser.parse("int reused;").unwrap().root_node().has_error());
}

#[test]
fn native_trait_discards_previously_interrupted_parse() {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&c_language()).unwrap();
    let source = "int first;\n".repeat(1000);
    let mut cancel = |_: &tree_sitter::ParseState| ControlFlow::Break(());
    assert!(
        parser
            .parse_with_options(
                &mut |byte, _| &source.as_bytes()[byte..],
                None,
                Some(tree_sitter::ParseOptions::new().progress_callback(&mut cancel))
            )
            .is_none()
    );
    let tree = Parse::parse(&mut parser, "int other;").unwrap();
    assert_eq!(tree.root_node().byte_range(), 0..10);
    assert!(!tree.root_node().has_error());
}

#[test]
fn options_reborrow_preserves_callback_and_pack_settings() {
    let language = Language::new(&c_language()).unwrap();
    let mut parser = compatible(&language);
    let mut callbacks = 0;
    let mut progress = |_: &dyn ParseStateLike| {
        callbacks += 1;
        ControlFlow::Continue(())
    };
    let mut options = PackedParseOptions {
        parse: ParseOptions::new().progress_callback(&mut progress),
        pack: PackOptions {
            points: false,
            symbol_presence: false,
            ..Default::default()
        },
    };
    for source in ["int first;", "int second;"] {
        let tree = parser
            .parse_with_options(source, options.reborrow())
            .unwrap();
        assert!(tree.point_data().is_none());
        assert!(tree.presence_cache().is_none());
    }
    assert!(callbacks >= 2);
}
