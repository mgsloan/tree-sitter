mod support;

use std::ops::ControlFlow;
use support::{assert_same_tree, c_language, json_language};
use tree_squatter::{
    Error, Forest, Language, PackOptions, PackedParseOptions, ParseOptions, Parser, ParserError,
    TreeFellerParser,
    traits::{NodeLike, Parse, ParseStateLike, TreeLike},
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
            &mut |byte, _| &source.as_bytes()[byte..],
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
            .parse_with_options(&mut |_, _| b"" as &[u8], Default::default())
            .unwrap_err(),
        ParserError::NoLanguage
    );
    let mut native = tree_sitter::Parser::new();
    assert_eq!(
        Parse::parse(&mut native, "x").unwrap_err(),
        ParserError::NoLanguage
    );
    assert_eq!(
        Parse::parse_with_options(&mut native, &mut |_, _| b"" as &[u8], Default::default())
            .unwrap_err(),
        ParserError::NoLanguage
    );

    let language = Language::new(&c_language()).unwrap();
    parser.set_language(&language).unwrap();
    native.set_language(&c_language()).unwrap();
    let mut direct = TreeFellerParser::new(&language).unwrap();
    assert_eq!(direct.language().tree_sitter_language(), c_language());
    check_generic(&mut parser);
    check_generic(&mut native);
    check_generic(&mut direct);
    let source = b"int value;\n";
    assert_same_tree(
        &callback_tree(&mut direct, source),
        &Parse::parse(&mut direct, source).unwrap(),
    );
}

#[test]
fn direct_traits_ignore_progress_callbacks() {
    let language = Language::new(&c_language()).unwrap();
    let mut parser = TreeFellerParser::new(&language).unwrap();
    let mut calls = 0;
    let mut cancel = |_: &dyn ParseStateLike| {
        calls += 1;
        ControlFlow::Break(())
    };
    let mut options = PackedParseOptions {
        parse: ParseOptions::new().progress_callback(&mut cancel),
        pack: PackOptions {
            points: false,
            symbol_presence: &|_| false,
            ..Default::default()
        },
    };
    let source = b"int value;\n";
    let contiguous = Parse::parse_with_options(
        &mut parser,
        &mut |byte, _| &source[byte..],
        options.reborrow(),
    )
    .unwrap();
    let chunked = Parse::parse_with_options(
        &mut parser,
        &mut |byte, _| source[byte..(byte + 2).min(source.len())].to_vec(),
        options,
    )
    .unwrap();
    assert_eq!(calls, 0);
    for tree in [&contiguous, &chunked] {
        assert_eq!(tree.root_node().byte_range(), 0..source.len());
        assert!(tree.point_data().is_none());
        assert!(tree.presence_cache().is_none());
    }
    assert_same_tree(&chunked, &contiguous);
}

fn callback_tree<P: Parse>(parser: &mut P, source: &[u8]) -> P::Tree
where
    P::Error: std::fmt::Debug,
{
    let mut reads = 0;
    let tree = parser
        .parse_with_options(
            &mut |byte, point| {
                reads += 1;
                check_point(source, byte, point);
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

    let mut progress = |state: &dyn ParseStateLike| {
        assert!(!state.is_converting());
        ControlFlow::Continue(())
    };
    let recovered = parser
        .parse_with_options(
            &mut |byte, _| &b"{broken"[byte..],
            ParseOptions::new().progress_callback(&mut progress).into(),
        )
        .unwrap();
    assert!(recovered.root_node().has_error());
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
    let grammar = parser.language().unwrap();
    let mut native = tree_sitter::Parser::new();
    native.set_language(&c_language()).unwrap();
    assert!(
        Forest::parse(grammar, &mut native, "int broken = ;")
            .unwrap()
            .root_node()
            .has_error()
    );
    assert_eq!(
        Forest::parse_direct(grammar, "int broken = ;")
            .unwrap_err()
            .code,
        Error::Parse
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

fn check_point(source: &[u8], byte: usize, point: tree_sitter::Point) {
    assert!(byte <= source.len());
    let prefix = &source[..byte];
    let row = prefix.iter().filter(|&&value| value == b'\n').count();
    let column = prefix
        .iter()
        .rposition(|&value| value == b'\n')
        .map_or(byte, |newline| byte - newline - 1);
    assert_eq!(point, tree_sitter::Point::new(row, column));
}

#[test]
fn direct_callback_chunks_match_contiguous() {
    let sources = [
        "".to_owned(),
        " \n\t".into(),
        "\u{feff}/* π😀€ */\nint value; /* trailing */\n".into(),
        "int f(void) { return sizeof(T) + (T) * value; }\n".into(),
        "typedef struct { int member; } Item;\nint f(Item *item) { return item->member; }".into(),
        format!("char *text = \"{}\";\n", "x😀π".repeat(200)),
        "int f(void) { T(a); T *b; return (T)(a) + sizeof(T); }\n".into(),
    ];
    let language = Language::new(&c_language()).unwrap();
    let mut parser = TreeFellerParser::new(&language).unwrap();
    let mut compatible = compatible(&language);
    for source in &sources {
        for options in [
            PackOptions::default(),
            PackOptions {
                repack: true,
                points: false,
                symbol_presence: &|_| false,
                ..Default::default()
            },
        ] {
            let expected = Forest::parse_direct_with_options(&language, source, options).unwrap();
            let packed = compatible
                .parse_with_options(
                    &mut |byte, _| &source.as_bytes()[byte..],
                    PackedParseOptions {
                        pack: options,
                        ..Default::default()
                    },
                )
                .unwrap();
            assert_same_tree(&expected, &packed);
            for chunk_size in [1, 2, 3, 4, 7, 32, 4096] {
                let source = source.as_bytes();
                let actual = parser
                    .parse_with_options(
                        &mut |byte, point| {
                            check_point(source, byte, point);
                            // Fixed boundaries exercise suffix reads within rope leaves.
                            let end = ((byte / chunk_size + 1) * chunk_size).min(source.len());
                            source[byte..end].to_vec()
                        },
                        PackedParseOptions {
                            pack: options,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                assert_same_tree(&actual, &expected);
            }
        }
    }

    // Borrowed chunks stay borrowed and an entire input needs just one read plus EOF.
    let source = b"int first;\nint second;\n";
    let mut reads = 0;
    parser
        .parse_with_options(
            &mut |byte, point| {
                check_point(source, byte, point);
                reads += 1;
                &source[byte..]
            },
            PackedParseOptions::default(),
        )
        .unwrap();
    assert_eq!(reads, 2);
}

#[test]
fn direct_callback_failures_and_reuse() {
    let mut parser = {
        let language = Language::new(&c_language()).unwrap();
        TreeFellerParser::new(&language).unwrap()
    };
    let first = parser.parse("int before;").unwrap();
    let failure = parser.parse("int x;\n@").unwrap_err();
    assert_eq!(failure.code, Error::Parse);
    assert_eq!(failure.byte, 7);
    assert_eq!(failure.point, tree_sitter::Point::new(1, 0));
    assert_eq!(
        parser.parse("int broken = ;").unwrap_err().code,
        Error::Parse
    );
    for source in [
        &b"int x;\n@"[..],
        &b"int broken = ;"[..],
        &b"/* \xf0\x9f"[..],
        &b"int \xe2\n\xa0;"[..],
        &b"int \0;"[..],
    ] {
        let expected = parser.parse(source);
        for chunk_size in 1..=4 {
            let actual = parser.parse_with_options(
                &mut |byte, point| {
                    check_point(source, byte, point);
                    source[byte..(byte + chunk_size).min(source.len())].to_vec()
                },
                PackedParseOptions::default(),
            );
            match (&actual, &expected) {
                (Ok(actual), Ok(expected)) => assert_same_tree(actual, expected),
                (Err(actual), Err(expected)) => assert_eq!(actual, expected),
                _ => panic!("chunked and contiguous results differ: {source:?}"),
            }
        }
    }
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        parser.parse_with_options(
            &mut |byte, _| {
                assert!(byte == 0, "input callback panic");
                b"int value;".to_vec()
            },
            PackedParseOptions::default(),
        )
    }));
    assert!(panic.is_err());
    parser.parse("int reused;").unwrap();
    parser.drop_scratch();
    parser
        .parse_with_options(
            &mut |byte, _| &b"int reused;"[byte..],
            PackedParseOptions::default(),
        )
        .unwrap();
    let after = parser.parse("int f(void) { return 1; }").unwrap();
    drop(parser);
    assert_eq!(first.root_node().byte_range(), 0..11);
    assert_eq!(
        first
            .root_node()
            .named_child(tree_squatter::NamedChildIx::new(0))
            .unwrap()
            .kind(),
        "declaration"
    );
    assert_eq!(
        after
            .root_node()
            .named_child(tree_squatter::NamedChildIx::new(0))
            .unwrap()
            .kind(),
        "function_definition"
    );
}

fn check_packed<P>(parser: &mut P, source: &str) -> Forest
where
    P: Parse<Tree = Forest>,
    for<'a> P: Parse<Options<'a> = PackedParseOptions<'a>>,
    P::Error: std::fmt::Debug,
{
    let mut parsing = false;
    let mut progress = |state: &dyn ParseStateLike| {
        assert_eq!(state.is_converting(), state.current_byte_offset_descends());
        assert!(!state.has_error());
        assert!(!state.is_converting());
        parsing = true;
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
        .parse_with_options(
            &mut |byte, _| &source.as_bytes()[byte..],
            options.reborrow(),
        )
        .unwrap();
    assert!(parsing);
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
            &mut |byte, _| &source.as_bytes()[byte..],
            PackedParseOptions {
                pack: PackOptions {
                    initial_group_capacity: 1,
                    repack: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    assert_same_tree(&packed, &direct);

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
            symbol_presence: &|_| false,
            ..Default::default()
        },
    };
    for source in ["int first;", "int second;"] {
        let source = source.repeat(1000);
        let tree = parser
            .parse_with_options(
                &mut |byte, _| &source.as_bytes()[byte..],
                options.reborrow(),
            )
            .unwrap();
        assert!(tree.point_data().is_none());
        assert!(tree.presence_cache().is_none());
    }
    assert!(callbacks >= 2);
}

fn check_cancellation<P: Parse<Error = ParserError>>(parser: &mut P, source: &str) {
    let mut reports = 0;
    let mut progress = |state: &dyn ParseStateLike| {
        assert!(!state.is_converting());
        reports += 1;
        ControlFlow::Continue(())
    };
    parser
        .parse_with_options(
            &mut |byte, _| &source.as_bytes()[byte..],
            ParseOptions::new().progress_callback(&mut progress).into(),
        )
        .unwrap();
    assert!(reports > 3);
    for stop_at in [1, reports / 2, reports - 1, reports] {
        let mut count = 0;
        let mut progress = |state: &dyn ParseStateLike| {
            assert!(!state.is_converting());
            count += 1;
            if count == stop_at {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        };
        assert_eq!(
            parser
                .parse_with_options(
                    &mut |byte, _| &source.as_bytes()[byte..],
                    ParseOptions::new().progress_callback(&mut progress).into()
                )
                .err(),
            Some(ParserError::Canceled),
        );
        assert_eq!(count, stop_at);
        let tree = Parse::parse(parser, "int after;").unwrap();
        assert_eq!(tree.root_node().byte_range(), 0..10);
        assert!(!tree.root_node().has_error());
    }
}

#[test]
fn cancellation_and_packing_failure_allow_reuse() {
    let language = Language::new(&c_language()).unwrap();
    let source = "int value = 123;\n".repeat(1000);
    let mut parser = compatible(&language);
    check_cancellation(&mut parser, &source);
    let mut native = tree_sitter::Parser::new();
    native.set_language(&c_language()).unwrap();
    check_cancellation(&mut native, &source);

    let options = PackedParseOptions {
        pack: PackOptions {
            initial_group_capacity: u32::MAX,
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        parser
            .parse_with_options(&mut |byte, _| &b"int value;"[byte..], options)
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
