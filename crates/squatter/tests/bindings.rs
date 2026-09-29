mod support;

use std::error::Error;
use tree_squatter::{FieldId, GrammarId, KindId, SquatterGrammarId, SquatterKindId};
use tree_squatter::{
    Forest, KindSet, PackOptions,
    traits::{CursorLike, NodeLike},
};

use support::{
    assert_same_tree, c_language, json_language, native_query_results, parse_native, query_results,
};

#[test]
fn error_flags_match_each_native_node() -> Result<(), Box<dyn Error>> {
    let wide_array = format!("[{}", vec!["1"; 100].join(","));
    for (language, source) in [
        (json_language(), wide_array.as_str()),
        (json_language(), r#"{"good": 1, "bad": [2, ?]}"#),
        (c_language(), "int f(void) { return (1 + ); }"),
    ] {
        let native = parse_native(&language, source);
        let expected: Vec<_> = NodeLike::preorder(native.root_node())
            .map(|node| {
                (
                    KindId::from_raw(node.kind_id()),
                    node.byte_range(),
                    node.has_error(),
                )
            })
            .collect();
        assert!(expected.iter().any(|node| node.2));
        assert!(expected.iter().any(|node| !node.2));
        let grammar = tree_squatter::Language::new(&language)?;
        for points in [false, true] {
            let mut tree = Forest::pack_with_options(
                &grammar,
                &native,
                PackOptions {
                    initial_group_capacity: 1,
                    points,
                    ..Default::default()
                },
            )?;
            tree.repack_in_place()?;
            let compact = tree.repack()?;
            let copy = Forest::from_bytes(std::slice::from_ref(&grammar), compact.as_bytes())?;
            let borrowed =
                Forest::from_bytes_borrowed(std::slice::from_ref(&grammar), compact.as_bytes())?;
            for tree in [&tree, &compact, &copy, &*borrowed] {
                let actual: Vec<_> = tree
                    .root_node()
                    .preorder()
                    .nodes()
                    .map(|node| (node.kind_id(), node.byte_range(), node.has_error()))
                    .collect();
                assert_eq!(actual, expected, "{source}");
            }
        }
    }
    Ok(())
}

fn check_shared_navigation<'tree, N: NodeLike<'tree>>(
    root: N,
    fields: u16,
) -> Result<(), Box<dyn Error>> {
    let mut cursor = root.walk();
    let expected: Vec<_> = root.preorder().collect();
    let all_kinds = KindSet::new(expected.iter().map(|node| node.kind_id()));
    assert!(
        root.descendants_matching_kinds(&all_kinds)
            .collect::<Vec<_>>()
            == expected
    );
    assert!(
        root.descendants_matching_kinds(&KindSet::default())
            .next()
            .is_none()
    );
    for node in expected.iter().step_by((expected.len() / 20).max(1)) {
        let kinds = KindSet::new([
            node.kind_id(),
            root.kind_id(),
            node.kind_id(),
            KindId::ERROR,
        ]);
        let filtered: Vec<_> = expected
            .iter()
            .copied()
            .filter(|node| kinds.contains(node.kind_id()))
            .collect();
        assert!(root.descendants_matching_kinds(&kinds).collect::<Vec<_>>() == filtered);
    }
    for &node in &expected {
        let attributes = node.attributes();
        assert_eq!(node.kind_id(), attributes.kind_id);
        assert_eq!(node.grammar_id(), attributes.grammar_id);
        assert_eq!(node.kind(), attributes.kind);
        assert_eq!(node.grammar_name(), attributes.grammar_name);
        assert_eq!(node.start_byte(), attributes.start_byte);
        assert_eq!(node.end_byte(), attributes.end_byte);
        assert_eq!(
            node.byte_range(),
            attributes.start_byte..attributes.end_byte
        );
        assert_eq!(node.start_position(), attributes.start_position);
        assert_eq!(node.end_position(), attributes.end_position);
        assert_eq!(node.is_named(), attributes.is_named);
        assert_eq!(node.is_extra(), attributes.is_extra);
        assert_eq!(node.is_missing(), attributes.is_missing);
        assert_eq!(node.is_error(), attributes.is_error);
        assert_eq!(node.has_error(), attributes.has_error);
        assert_eq!(node.has_children(), node.child_count().raw() != 0);
        assert_eq!(
            node.has_named_children(),
            node.named_child_count().raw() != 0
        );
    }
    for &node in expected.iter().take(16) {
        cursor.reset(node);
        assert_eq!(cursor.depth(), 0);
        assert!(!cursor.goto_parent());
        assert!(!cursor.goto_previous_sibling());
        assert!(!cursor.goto_next_sibling());
        let mut children = Vec::new();
        let mut child_fields = Vec::new();
        if cursor.goto_first_child() {
            loop {
                children.push(cursor.node());
                child_fields.push(cursor.field_id());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            for &child in children.iter().rev() {
                assert!(cursor.node() == child);
                let moved = cursor.goto_previous_sibling();
                assert_eq!(moved, child != children[0]);
            }
            assert!(cursor.goto_parent());
        }
        assert!(node.children(&mut cursor).collect::<Vec<_>>() == children);
        assert!(
            node.named_children(&mut cursor).collect::<Vec<_>>()
                == children
                    .iter()
                    .copied()
                    .filter(|node| node.is_named())
                    .collect::<Vec<_>>()
        );
        for field in 1..=fields {
            let field = FieldId::from_raw(field).unwrap();
            let filtered: Vec<_> = children
                .iter()
                .zip(&child_fields)
                .filter_map(|(&child, &actual)| (actual == Some(field)).then_some(child))
                .collect();
            assert!(node.child_by_field_id(field) == filtered.first().copied());
            assert!(
                node.children_by_field_id(field, &mut cursor)
                    .collect::<Vec<_>>()
                    == filtered
            );
        }
        for child in std::iter::once(node).chain(children.iter().copied().take(16)) {
            for byte in [child.start_byte(), child.end_byte(), usize::MAX] {
                cursor.reset(node);
                let expected_index = children.iter().position(|node| {
                    node.end_byte() > byte && node.end_position() > tree_sitter::Point::default()
                });
                assert_eq!(
                    cursor
                        .goto_first_child_for_byte(byte)
                        .map(|index| index.raw() as usize),
                    expected_index
                );
                assert!(cursor.node() == expected_index.map_or(node, |index| children[index]));
                assert_eq!(cursor.depth(), u32::from(expected_index.is_some()));
            }
            for point in [
                child.start_position(),
                child.end_position(),
                tree_sitter::Point::new(u32::MAX as usize, 0),
            ] {
                cursor.reset(node);
                let expected_index = children
                    .iter()
                    .position(|node| node.end_byte() > 0 && node.end_position() > point);
                assert_eq!(
                    cursor
                        .goto_first_child_for_point(point)
                        .map(|index| index.raw() as usize),
                    expected_index
                );
                assert!(cursor.node() == expected_index.map_or(node, |index| children[index]));
            }
        }
        let descendants: Vec<_> = node.preorder().collect();
        assert!(
            node.descendants_matching_kinds(&all_kinds)
                .collect::<Vec<_>>()
                == descendants
        );
    }
    Ok(())
}

fn check_queries(
    language: &tree_sitter::Language,
    source: &[u8],
    mainline: &tree_sitter::Tree,
    packed: &Forest,
) -> Result<(), Box<dyn Error>> {
    let grammar = tree_squatter::Language::new(language)?;
    for source_query in [
        "(_) @node",
        "(_) @a (_) @b",
        "(_ . (_) @child) @parent",
        "((_) @text (#eq? @text \"true\"))",
        "((_) @text (#not-eq? @text \"true\"))",
        "((_) @text (#match? @text \"^[0-9]+$\"))",
        "((_) @text (#not-match? @text \"[a-z]\"))",
        "((_) @text (#any-of? @text \"1\" \"2\"))",
        "((_) @text (#not-any-of? @text \"1\" \"2\"))",
        "(_ (_) @a (_) @b (#eq? @a @b))",
        "(_ (_)+ @a (#any-eq? @a \"true\"))",
    ] {
        for modification in [0, 2] {
            let mut expected_query = tree_sitter::Query::new(language, source_query)?;
            let mut actual_query = tree_squatter::Query::new(&grammar, source_query)?;
            if modification == 2 {
                let name = actual_query.capture_names()[0].to_owned();
                expected_query.disable_capture(&name);
                actual_query.disable_capture(&name);
            }
            for captures in [false, true] {
                let expected =
                    native_query_results(&expected_query, mainline.root_node(), source, captures);
                let mut actual_cursor = tree_squatter::QueryCursor::new();
                let actual = query_results(
                    &mut actual_cursor,
                    &actual_query,
                    packed.root_node(),
                    source,
                    captures,
                );
                assert_eq!(
                    expected, actual,
                    "{source_query}; captures={captures}; modification={modification}"
                );
            }
        }
    }
    Ok(())
}

// Reusing a cursor must not inspect the query/tree/options from its previous
// execution. Those borrows end when QueryExecution is dropped.
fn check_cursor_reuse(
    language: &tree_sitter::Language,
    tree: &tree_sitter::Tree,
) -> Result<(), Box<dyn Error>> {
    use tree_squatter::{Query, QueryCursor};
    let grammar = tree_squatter::Language::new(language)?;
    let mut cursor = QueryCursor::new();
    for _ in 0..3 {
        let packed = Forest::pack(&grammar, tree)?;
        let query = Query::new(&grammar, "(_) @node")?;
        let mut execution = cursor.execute(&query, packed.root_node(), b"".as_slice());
        assert!(execution.next_capture().is_some());
    }
    // This checkout's mainline disable_pattern leaves the wildcard-root count
    // stale and asserts. Verify the intended behavior directly for this case.
    let packed = Forest::pack(&grammar, tree)?;
    let mut query = Query::new(&grammar, "(_) @a (_) @b")?;
    query.disable_pattern(tree_squatter::PatternIx(0));
    {
        let mut execution = cursor.execute(&query, packed.root_node(), b"".as_slice());
        let mut count = 0;
        while let Some(result) = execution.next_match() {
            assert_eq!(result.pattern_index.0, 1);
            assert_eq!(result.captures().len(), 1);
            assert_eq!(result.captures()[0].index.0, 1);
            count += 1;
        }
        assert_eq!(
            count,
            packed
                .root_node()
                .preorder()
                .nodes()
                .filter(|node| node.is_named() && !node.is_error())
                .count()
        );
    }
    query.disable_pattern(tree_squatter::PatternIx(1));
    assert!(
        cursor
            .execute(&query, packed.root_node(), b"".as_slice())
            .next_match()
            .is_none()
    );
    let query = Query::new(&grammar, "(_ (_)+ @child) @parent")?;
    cursor.set_byte_range(1..12);
    {
        let mut execution = cursor.execute(&query, packed.root_node(), b"".as_slice());
        assert!(execution.next_match().is_some());
        assert_eq!(execution.error(), None);
    }
    cursor.set_byte_range(0..0); // Zero end restores the unbounded range.
    let mut execution = cursor.execute(&query, packed.root_node(), b"".as_slice());
    assert!(execution.next_match().is_some());
    assert_eq!(execution.error(), None);
    Ok(())
}

const SOURCE: &str = "{\"a\": [1, true, null], \"b\": 2}";

fn fixture() -> Result<(tree_sitter::Language, tree_sitter::Tree, Forest), Box<dyn Error>> {
    let language = json_language();
    let grammar = tree_squatter::Language::new(&language)?;
    let native = parse_native(&language, SOURCE);
    let packed = Forest::pack_with_options(
        &grammar,
        &native,
        PackOptions {
            initial_group_capacity: 1,
            ..Default::default()
        },
    )?;
    Ok((language, native, packed))
}

#[test]
fn shared_navigation() -> Result<(), Box<dyn Error>> {
    let (language, native, packed) = fixture()?;
    check_shared_navigation(native.root_node(), language.field_count() as u16)?;
    assert_eq!(packed.root_node().field_id(), None);
    check_shared_navigation(packed.root_node(), language.field_count() as u16)?;
    let grammar = tree_squatter::Language::new(&language)?;
    // Cross the presence-index threshold and several physical groups, retaining
    // a rare boolean beside common number and punctuation symbols.
    let source = format!("[true,{}null]", "123,\n".repeat(600));
    let native = parse_native(&language, &source);
    for points in [false, true] {
        for symbol_presence in [false, true] {
            let packed = Forest::pack_with_options(
                &grammar,
                &native,
                PackOptions {
                    points,
                    symbol_presence: &|_| symbol_presence,
                    ..Default::default()
                },
            )?;
            check_shared_navigation(packed.root_node(), language.field_count() as u16)?;
        }
    }
    Ok(())
}

#[test]
fn streaming_queries_and_cursor_reuse() -> Result<(), Box<dyn Error>> {
    let (language, native, packed) = fixture()?;
    check_queries(&language, SOURCE.as_bytes(), &native, &packed)?;
    check_cursor_reuse(&language, &native)?;
    Ok(())
}

#[test]
fn owned_and_borrowed_storage() -> Result<(), Box<dyn Error>> {
    let (language, native, packed) = fixture()?;
    let grammar = tree_squatter::Language::new(&language)?;
    let compact = packed.repack()?;
    let decoded = Forest::from_bytes(std::slice::from_ref(&grammar), compact.as_bytes())?;
    let borrowed = Forest::from_bytes_borrowed(std::slice::from_ref(&grammar), compact.as_bytes())?;
    assert_eq!(borrowed.as_bytes().as_ptr(), compact.as_bytes().as_ptr());
    assert_eq!(
        borrowed.root_node().byte_range(),
        compact.root_node().byte_range()
    );
    assert_eq!(borrowed.root_node().kind(), compact.root_node().kind());
    let expected: Vec<_> = compact
        .root_node()
        .preorder()
        .nodes()
        .map(|node| (node.kind().to_owned(), node.byte_range()))
        .collect();
    assert_eq!(compact.group_count(), compact.group_capacity());
    drop(borrowed);
    drop(compact);
    drop(packed);
    drop(native);
    drop(grammar);
    assert_eq!(
        decoded
            .root_node()
            .preorder()
            .nodes()
            .map(|node| (node.kind().to_owned(), node.byte_range()))
            .collect::<Vec<_>>(),
        expected
    );
    Ok(())
}

#[test]
fn direct_parser_matches_mainline_packing() -> Result<(), Box<dyn Error>> {
    use tree_squatter::{Language, PackedParseOptions, TreeFellerParser};

    let language = c_language();
    let grammar = Language::new(&language)?;
    let mut mainline = tree_sitter::Parser::new();
    mainline.set_language(&language)?;
    let mut direct_parser = TreeFellerParser::new(&grammar)?;
    let mut sources = vec![
        String::new(),
        "/* comment only */\n".into(),
        "int x; /* trailing */".into(),
        "int x = 1 + 2;".into(),
        "/* π */ typedef struct { int member; } Item;\n\
         int f(Item *item) { return item->member + 1; } /* end */"
            .into(),
        format!("char *text = \"{}\";\n", "x".repeat(700)),
        format!(
            "int f(void) {{ return {}1{}; }}",
            "(".repeat(300),
            ")".repeat(300)
        ),
    ];
    sources.push(
        (0..300)
            .map(|index| format!("int value{index} = {index};\n"))
            .collect(),
    );
    for source in sources {
        let native = mainline.parse(&source, None).ok_or("parse failed")?;
        assert!(!native.root_node().has_error());
        for points in [false, true] {
            for symbol_presence in [false, true] {
                let options = PackOptions {
                    initial_group_capacity: 1,
                    repack: true,
                    symbol_presence: &|_| symbol_presence,
                    points,
                    ..Default::default()
                };
                let direct = direct_parser.parse_with_options(
                    &mut |byte, _| &source.as_bytes()[byte..],
                    PackedParseOptions {
                        pack: options,
                        ..Default::default()
                    },
                )?;
                let expected = Forest::pack_with_options(&grammar, &native, options)?;
                assert_same_tree(&direct, &expected);
            }
        }
    }
    let tree = Forest::parse_direct(&grammar, "int direct;")?;
    assert_eq!(tree.root_node().byte_range(), 0..11);
    Forest::from_bytes(std::slice::from_ref(&grammar), tree.as_bytes())?;
    Ok(())
}

#[test]
fn direct_parser_external_scanner_matches_python() -> Result<(), Box<dyn Error>> {
    use tree_squatter::{Language, PackedParseOptions, TreeFellerParser};

    let language = unsafe {
        tree_sitter::Language::from_raw(tree_sitter_python::LANGUAGE.into_raw()().cast())
    };
    let grammar = Language::new(&language)?;
    let mut mainline = tree_sitter::Parser::new();
    mainline.set_language(&language)?;
    let mut parser = TreeFellerParser::new(&grammar)?;
    for source in [
        "",
        "# comment only\n",
        "if True:\n    if False:\n        pass\n    else:\n        pass\nx = 1\n",
        "def f(value):\n\treturn f'hello {value!r:>10} π😀'\n",
        "text = f'outer {f\"inner {value}\"} end'\n",
        "text = r'''multiline\nπ😀 \"quoted\" text\n'''\n",
        "text = f'''multiline\n{value + 1}\n'''\n",
        "values = [\n  item for item in source\n  if item\n]\n",
        "if True:\n    # comment\n    pass",
        "if True:\n    if True:\n        pass",
        "text = 'a' \\\n    'b'\n",
        "match value:\n    case [first, *rest]:\n        pass\n",
    ] {
        let native = mainline.parse(source, None).ok_or("parse failed")?;
        assert!(!native.root_node().has_error(), "{source}");
        let expected = Forest::pack(&grammar, &native)?;
        let actual = parser.parse(source)?;
        for (left, right) in actual
            .root_node()
            .preorder()
            .nodes()
            .zip(expected.root_node().preorder().nodes())
        {
            assert_eq!(
                (left.kind(), left.byte_range()),
                (right.kind(), right.byte_range()),
                "{source:?}"
            );
        }
        assert_same_tree(&actual, &expected);
        for chunk_size in [1, 3, 8] {
            let actual = parser.parse_with_options(
                &mut |byte, _| {
                    source.as_bytes()[byte..(byte + chunk_size).min(source.len())].to_vec()
                },
                PackedParseOptions::default(),
            )?;
            assert_same_tree(&actual, &expected);
        }
    }
    assert!(parser.parse("text = f'unterminated {").is_err());
    assert!(parser.parse("text = f'fresh {value}'").is_ok());
    Ok(())
}

#[test]
fn direct_parser_rejects_unsupported_grammar() -> Result<(), Box<dyn Error>> {
    use tree_squatter::{Error as SquatError, Language};

    // This dependency generates ABI 14, which remains usable by the conversion
    // API but must never silently fall back to a mainline parser.
    let language = json_language();
    let grammar = Language::new(&language)?;
    let failure = tree_squatter::TreeFellerParser::new(&grammar)
        .err()
        .ok_or("accepted ABI 14")?;
    assert_eq!(failure.code, SquatError::Language);
    assert_eq!(
        Forest::parse_direct(&grammar, SOURCE).unwrap_err().code,
        SquatError::Language
    );
    Ok(())
}

#[test]
fn language_inspection_matches_native() {
    for native in [json_language(), c_language()] {
        let language = tree_squatter::Language::new(&native).unwrap();
        assert_eq!(language.is_parseable(), native.is_parseable());
        assert_eq!(language.name(), native.name());
        assert_eq!(language.abi_version(), native.abi_version());
        let version = |metadata: tree_sitter::LanguageMetadata| {
            (
                metadata.major_version,
                metadata.minor_version,
                metadata.patch_version,
            )
        };
        assert_eq!(
            language.metadata().map(version),
            native.metadata().map(version)
        );
        assert_eq!(language.node_kind_count(), native.node_kind_count());
        assert_eq!(language.parse_state_count(), native.parse_state_count());
        assert_eq!(language.field_count(), native.field_count());
        assert_eq!(
            language
                .supertypes()
                .iter()
                .map(|id| id.raw())
                .collect::<Vec<_>>(),
            native.supertypes()
        );
        for &supertype in language.supertypes() {
            assert_eq!(
                language
                    .subtypes_for_supertype(supertype)
                    .iter()
                    .map(|id| id.raw())
                    .collect::<Vec<_>>(),
                native.subtypes_for_supertype(supertype.raw())
            );
        }
        for raw in (0..native.node_kind_count() as u16).chain([u16::MAX - 1, u16::MAX]) {
            let id = KindId::from_raw(raw);
            assert_eq!(language.node_kind_for_id(id), native.node_kind_for_id(raw));
            assert_eq!(
                language.node_kind_is_named(id),
                native.node_kind_is_named(raw)
            );
            assert_eq!(
                language.node_kind_is_visible(id),
                native.node_kind_is_visible(raw)
            );
            assert_eq!(
                language.node_kind_is_supertype(id),
                native.node_kind_is_supertype(raw)
            );
            if let Some(name) = native.node_kind_for_id(raw) {
                for named in [false, true] {
                    assert_eq!(
                        language.id_for_node_kind(name, named).raw(),
                        native.id_for_node_kind(name, named)
                    );
                }
            }
        }
        assert_eq!(
            language.id_for_node_kind("unknown-kind", true),
            KindId::from_raw(0)
        );
        assert_eq!(language.kind_id_for_name("unknown-kind", true), None);
        for raw in 1..=native.field_count() as u16 {
            let id = FieldId::from_raw(raw).unwrap();
            let name = language.field_name_for_id(id).unwrap();
            assert_eq!(Some(name), native.field_name_for_id(raw));
            assert_eq!(language.field_id_for_name(name.as_bytes()), Some(id));
        }
        assert_eq!(language.field_id_for_name([255]), None);
        assert_eq!(FieldId::from_raw(0), None);
        assert_eq!(language.field_id_for_name("unknown"), None);
        assert_eq!(
            language.kind_id_for_name("ERROR", true),
            Some(KindId::ERROR)
        );
        assert_eq!(language.grammar_id_for_name("unknown", true), None);
        assert_eq!(
            language.grammar_id_for_name("ERROR", true).unwrap().raw(),
            u16::MAX
        );
        if native == json_language() {
            assert_eq!(language.kind_id_for_name("string", false), None);
        } else {
            assert_eq!(language.grammar_id_for_name("type_identifier", true), None);
            assert_eq!(language.grammar_id_for_name("identifier", false), None);
        }
    }
}

#[test]
fn compact_ids_roundtrip_native_kinds_and_scans() -> Result<(), Box<dyn Error>> {
    for (native, source) in [
        (json_language(), r#"{"good": 1, "bad": [2, ?]}"#),
        (
            c_language(),
            "typedef int T; T value; struct Point { int x; }; int f(void) { return (1 + ); }",
        ),
    ] {
        let language = tree_squatter::Language::new(&native)?;
        let parsed = parse_native(&native, source);
        let tree = Forest::pack(&language, &parsed)?;
        assert!(language.squatter_kind_count() < language.node_kind_count() + 2);
        for raw in 1..language.squatter_kind_count() as u16 {
            let compact = SquatterKindId::from_raw(raw);
            let native = language.kind_id(compact).unwrap();
            assert_eq!(language.squatter_kind_id(native), Some(compact));
        }
        for raw in 1..language.squatter_grammar_count() as u16 {
            let compact = SquatterGrammarId::from_raw(raw);
            let native = language.grammar_id(compact).unwrap();
            assert_eq!(language.squatter_grammar_id(native), Some(compact));
        }
        for (node, expected) in tree
            .root_node()
            .preorder()
            .nodes()
            .zip(NodeLike::preorder(parsed.root_node()))
        {
            assert_eq!(tree.node_at_slot(node.slot()), Some(node));
            if node.kind() == "type_identifier" {
                assert_eq!(
                    node.grammar_id(),
                    language.grammar_id_for_name("identifier", true).unwrap()
                );
            }
            let kind = node.squatter_kind_id();
            let grammar = node.squatter_grammar_id();
            assert_eq!(
                language.kind_id(kind),
                Some(KindId::from_raw(expected.kind_id()))
            );
            assert_eq!(
                language.grammar_id(grammar),
                Some(GrammarId::from_raw(expected.grammar_id()))
            );
            assert_eq!(
                language.squatter_kind_id_for_name(node.kind(), node.is_named()),
                Some(kind)
            );
            assert_eq!(
                tree.root_node()
                    .all()
                    .filter_squatter_kind_ids([
                        kind,
                        SquatterKindId::from_raw(0),
                        SquatterKindId::from_raw(u16::MAX)
                    ])
                    .nodes()
                    .collect::<Vec<_>>(),
                tree.root_node()
                    .all()
                    .filter_kind_ids([node.kind_id()])
                    .nodes()
                    .collect::<Vec<_>>()
            );
        }
        let name = tree.root_node().grammar_name();
        assert_eq!(
            language.squatter_grammar_id_for_name(name, true),
            Some(tree.root_node().squatter_grammar_id())
        );
    }
    Ok(())
}

#[test]
fn tree_views_and_text_access() {
    let language = json_language();
    let grammar = tree_squatter::Language::new(&language).unwrap();
    let source: Vec<u16> = "[\n\"😀\", 42]".encode_utf16().collect();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse_utf16_le(&source, None).unwrap();
    for points in [false, true] {
        let tree = Forest::pack_with_options(
            &grammar,
            &native,
            PackOptions {
                points,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(tree.language().abi_version(), language.abi_version());
        assert!(std::ptr::eq(tree.root_node().language(), tree.language()));
        assert_eq!(tree.walk().node(), tree.root_node());
        for (node, expected) in tree
            .root_node()
            .preorder()
            .nodes()
            .zip(NodeLike::preorder(native.root_node()))
        {
            assert_eq!(node.utf16_text(&source), expected.utf16_text(&source));
            assert_eq!(
                NodeLike::utf16_text(&node, &source),
                expected.utf16_text(&source)
            );
            let range = node.range();
            assert_eq!(range.start_byte, expected.start_byte());
            assert_eq!(range.end_byte, expected.end_byte());
            assert_eq!(NodeLike::range(&node), range);
            if points {
                assert_eq!(range, expected.range());
            } else {
                assert_eq!(
                    range.start_point,
                    tree_sitter::Point::new(0, range.start_byte)
                );
                assert_eq!(range.end_point, tree_sitter::Point::new(0, range.end_byte));
            }
        }
    }
}
