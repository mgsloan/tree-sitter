mod support;

use std::error::Error;
use tree_squatter::{FieldId, KindId};
use tree_squatter::{
    KindSet, PackOptions, Tree,
    traits::{CursorLike, NodeLike},
};

use support::{c_language, describe_capture, json_language, native_query_results, parse_native};

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
            .map(|node| (node.kind_id(), node.byte_range(), node.has_error()))
            .collect();
        assert!(expected.iter().any(|node| node.2));
        assert!(expected.iter().any(|node| !node.2));
        let grammar = tree_squatter::Language::new(&language)?;
        for points in [false, true] {
            let mut tree = Tree::pack_with_options(
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
            let copy = Tree::from_bytes(&grammar, compact.as_bytes())?;
            let borrowed = Tree::from_bytes_borrowed(&grammar, compact.as_bytes())?;
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
    let mut cursor = root.cursor()?;
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
        assert_eq!(node.has_changes(), attributes.has_changes);
        assert_eq!(node.has_children(), node.child_count().get() != 0);
        assert_eq!(
            node.has_named_children(),
            node.named_child_count().get() != 0
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
        assert!(node.children().collect::<Vec<_>>() == children);
        assert!(
            node.named_children().collect::<Vec<_>>()
                == children
                    .iter()
                    .copied()
                    .filter(|node| node.is_named())
                    .collect::<Vec<_>>()
        );
        for field in 1..=fields {
            let field = FieldId::new(field).unwrap();
            let filtered: Vec<_> = children
                .iter()
                .zip(&child_fields)
                .filter_map(|(&child, &actual)| (actual == Some(field)).then_some(child))
                .collect();
            assert!(node.children_by_field_id(field).collect::<Vec<_>>() == filtered);
        }
        for child in std::iter::once(node).chain(children.iter().copied().take(16)) {
            for byte in [child.start_byte(), child.end_byte(), usize::MAX] {
                cursor.reset(node);
                let expected_index = children.iter().position(|node| {
                    node.end_byte() > byte && node.end_position() > tree_sitter::Point::default()
                });
                assert_eq!(cursor.goto_first_child_for_byte(byte), expected_index);
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
                assert_eq!(cursor.goto_first_child_for_point(point), expected_index);
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
    packed: &Tree,
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
                let name = actual_query.capture_names()[0].clone();
                expected_query.disable_capture(&name);
                actual_query.disable_capture(&name);
            }
            for captures in [false, true] {
                let expected =
                    native_query_results(&expected_query, mainline.root_node(), source, captures);
                let mut actual_cursor = tree_squatter::QueryCursor::new();
                let mut execution =
                    actual_cursor.execute(&actual_query, packed.root_node(), source);
                let mut actual = Vec::new();
                loop {
                    let next = if captures {
                        execution
                            .next_capture()
                            .map(|(result, index)| (result, Some(index)))
                    } else {
                        execution.next_match().map(|result| (result, None))
                    };
                    let Some((result, index)) = next else {
                        break;
                    };
                    actual.push((
                        result.pattern_index,
                        index,
                        result
                            .captures
                            .iter()
                            .map(|capture| {
                                describe_capture(
                                    capture.index,
                                    capture.node.kind_id(),
                                    capture.node.byte_range(),
                                )
                            })
                            .collect::<Vec<_>>(),
                    ));
                }
                assert_eq!(execution.error(), None);
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
    use tree_squatter::{Query, QueryCursor, QueryExecutionError};
    let grammar = tree_squatter::Language::new(language)?;
    let mut cursor = QueryCursor::new();
    cursor.set_timeout(Some(std::time::Duration::from_secs(1)));
    for _ in 0..3 {
        let packed = Tree::pack(&grammar, tree)?;
        let query = Query::new(&grammar, "(_) @node")?;
        let mut execution = cursor.execute(&query, packed.root_node(), b"");
        assert!(execution.next_capture().is_some());
    }
    // This checkout's mainline disable_pattern leaves the wildcard-root count
    // stale and asserts. Verify the intended behavior directly for this case.
    let packed = Tree::pack(&grammar, tree)?;
    let mut query = Query::new(&grammar, "(_) @a (_) @b")?;
    query.disable_pattern(0);
    {
        let mut execution = cursor.execute(&query, packed.root_node(), b"");
        let mut count = 0;
        while let Some(result) = execution.next_match() {
            assert_eq!(result.pattern_index, 1);
            assert_eq!(result.captures.len(), 1);
            assert_eq!(result.captures[0].index, 1);
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
    query.disable_pattern(1);
    assert!(
        cursor
            .execute(&query, packed.root_node(), b"")
            .next_match()
            .is_none()
    );
    let query = Query::new(&grammar, "(_ (_)+ @child) @parent")?;
    assert!(cursor.set_byte_range(1..12));
    {
        let mut execution = cursor.execute(&query, packed.root_node(), b"");
        assert!(execution.next_match().is_none());
        assert_eq!(
            execution.error(),
            Some(QueryExecutionError::UnsupportedRange)
        );
    }
    assert!(cursor.set_byte_range(0..0)); // Zero end restores the unbounded range.
    let mut execution = cursor.execute(&query, packed.root_node(), b"");
    assert!(execution.next_match().is_some());
    assert_eq!(execution.error(), None);
    Ok(())
}

const SOURCE: &str = "{\"a\": [1, true, null], \"b\": 2}";

fn fixture() -> Result<(tree_sitter::Language, tree_sitter::Tree, Tree), Box<dyn Error>> {
    let language = json_language();
    let grammar = tree_squatter::Language::new(&language)?;
    let native = parse_native(&language, SOURCE);
    let packed = Tree::pack_with_options(
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
    check_shared_navigation(packed.root_node(), language.field_count() as u16)?;
    Ok(())
}

#[test]
fn group_boundaries_and_optional_columns() -> Result<(), Box<dyn Error>> {
    let (language, _, _) = fixture()?;
    let grammar = tree_squatter::Language::new(&language)?;
    // Cross the presence-index threshold and several physical groups, retaining
    // a rare boolean beside common number and punctuation symbols.
    let source = format!("[true,{}null]", "123,\n".repeat(600));
    let native = parse_native(&language, &source);
    for points in [false, true] {
        for symbol_presence in [false, true] {
            let packed = Tree::pack_with_options(
                &grammar,
                &native,
                PackOptions {
                    points,
                    symbol_presence,
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
    let decoded = Tree::from_bytes(&grammar, compact.as_bytes())?;
    let borrowed = Tree::from_bytes_borrowed(&grammar, compact.as_bytes())?;
    assert_eq!(borrowed.as_bytes().as_ptr(), compact.as_bytes().as_ptr());
    check_shared_navigation(borrowed.root_node(), language.field_count() as u16)?;
    check_queries(&language, SOURCE.as_bytes(), &native, &borrowed)?;
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
    let mut corrupted = decoded.as_bytes().to_vec();
    corrupted[0] ^= 0x80;
    let grammar = tree_squatter::Language::new(&language)?;
    assert!(Tree::from_bytes(&grammar, &corrupted).is_err());
    Ok(())
}

#[test]
fn direct_parser_matches_mainline_packing() -> Result<(), Box<dyn Error>> {
    use tree_squatter::{Language, Parser};

    let language = c_language();
    let grammar = Language::new(&language)?;
    let mut mainline = tree_sitter::Parser::new();
    mainline.set_language(&language)?;
    let mut direct_parser = Parser::new(&grammar)?;
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
                    symbol_presence,
                    points,
                };
                let direct = direct_parser.parse_with_options(&source, options)?;
                let expected = Tree::pack_with_options(&grammar, &native, options)?;
                assert_eq!(direct.as_bytes(), expected.as_bytes());
                assert_eq!(
                    direct.point_data().map(|points| points.as_bytes()),
                    expected.point_data().map(|points| points.as_bytes())
                );
                assert_eq!(
                    direct.presence_cache().map(|cache| cache.as_bytes()),
                    expected.presence_cache().map(|cache| cache.as_bytes())
                );
                let loaded = Tree::from_bytes(&grammar, direct.as_bytes())?;
                check_shared_navigation(loaded.root_node(), language.field_count() as u16)?;
            }
        }
    }
    let tree = Tree::parse_direct(&grammar, "int direct;")?;
    assert_eq!(tree.root_node().byte_range(), 0..11);
    Ok(())
}

#[test]
fn direct_parser_reuses_after_failure_and_owns_grammar() -> Result<(), Box<dyn Error>> {
    use tree_squatter::{Error as SquatError, Language, Parser};

    let mut parser = {
        let grammar = Language::new(&c_language())?;
        Parser::new(&grammar)?
    };
    let first = parser.parse("int before;")?;
    let failure = parser.parse("int x;\n@").unwrap_err();
    assert_eq!(failure.code, SquatError::Parse);
    assert_eq!(failure.byte, 7);
    assert_eq!(failure.point, tree_sitter::Point::new(1, 0));
    let syntax_failure = parser.parse("int broken = ;").unwrap_err();
    assert_eq!(syntax_failure.code, SquatError::Parse);
    let after = parser.parse("int after;")?;
    assert_eq!(after.root_node().byte_range(), 0..10);
    parser.trim();
    let trimmed = parser.parse("int f(void) { return 1; }")?;
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
        trimmed
            .root_node()
            .named_child(tree_squatter::NamedChildIx::new(0))
            .unwrap()
            .kind(),
        "function_definition"
    );
    Ok(())
}

#[test]
fn direct_parser_rejects_unsupported_grammar() -> Result<(), Box<dyn Error>> {
    use tree_squatter::{Error as SquatError, Language};

    // This dependency generates ABI 14, which remains usable by the conversion
    // API but must never silently fall back to a mainline parser.
    let language = json_language();
    let grammar = Language::new(&language)?;
    let failure = tree_squatter::Parser::new(&grammar)
        .err()
        .ok_or("accepted ABI 14")?;
    assert_eq!(failure.code, SquatError::Language);
    assert_eq!(
        Tree::parse_direct(&grammar, SOURCE).unwrap_err().code,
        SquatError::Language
    );
    Ok(())
}

#[test]
fn mainline_parse_keeps_error_recovery() -> Result<(), Box<dyn Error>> {
    use tree_squatter::{Error as SquatError, Language};

    let language = c_language();
    let grammar = Language::new(&language)?;
    let mut mainline = tree_sitter::Parser::new();
    mainline.set_language(&language)?;
    let source = "int broken = ;";
    let recovered = Tree::parse(&grammar, &mut mainline, source)?;
    assert!(recovered.root_node().has_error());
    assert_eq!(
        Tree::parse_direct(&grammar, source).unwrap_err().code,
        SquatError::Parse
    );
    Ok(())
}
