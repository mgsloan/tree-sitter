use std::time::Duration;
use tree_squatter::{Grammar, Query, QueryCursor, Tree};

macro_rules! matches {
    ($cursor:expr, $query:expr, $tree:expr, $source:expr) => {{
        let mut execution = $cursor.execute($query, $tree.root_node(), $source.as_bytes());
        let mut results = Vec::new();
        while let Some(result) = execution.next_match() {
            results.push((
                result.pattern_index,
                result
                    .captures
                    .iter()
                    .map(|capture| (u32::from(capture.node.slot()), capture.index))
                    .collect::<Vec<_>>(),
            ));
            assert!(results.len() < 100_000, "unexpected match explosion");
        }
        assert!(execution.error().is_none(), "{:?}", execution.error());
        results.sort();
        results
    }};
}

macro_rules! captures {
    ($cursor:expr, $query:expr, $tree:expr, $source:expr) => {{
        let mut execution = $cursor.execute($query, $tree.root_node(), $source.as_bytes());
        let mut results = Vec::new();
        while let Some((result, index)) = execution.next_capture() {
            let capture = result.captures[index];
            results.push((
                result.pattern_index,
                u32::from(capture.node.slot()),
                capture.index,
            ));
            assert!(results.len() < 100_000, "unexpected capture explosion");
        }
        assert!(execution.error().is_none());
        results.sort();
        results.dedup();
        results
    }};
}

#[test]
fn queries_match_with_and_without_plans() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) };
    let grammar = Grammar::new(&language).unwrap();
    let reference_grammar = tree_squatter::Grammar::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    // Three ambiguous capture runs create enough states to exercise indexed deduplication.
    let source = "// before\nint alpha(int x, int y) { int a = 1; if (x) return beta(x, y, a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t, u, v, w, x, y, z); return y; }\n// after\nint bravo = 2;\nint other() { return beta(a, b); }";
    let native = parser.parse(source, None).unwrap();
    let tree = Tree::pack(&grammar, &native).unwrap();
    let reference = tree_squatter::Tree::pack(&reference_grammar, &native).unwrap();

    for pattern in [
        "(identifier) @identifier",
        "(_) @node",
        "[ (identifier) (number_literal) ] @value",
        "(argument_list . (identifier) @first . (identifier) @second .)",
        "(function_definition declarator: (function_declarator declarator: (identifier) @name) body: (compound_statement) @body)",
        "(declaration declarator: (init_declarator declarator: (identifier) @name value: (_) @value))",
        "((identifier) @name (#match? @name \"^[ab]\"))",
        "(argument_list (identifier)* @before (identifier)+ @after)",
        "(argument_list (identifier)* @before (identifier)* @middle (identifier)* @after)",
        "((comment)* @comments (declaration) @declaration)",
        "(compound_statement . (declaration)? @declaration . (return_statement) @return .)",
        "(call_expression function: (_) @call arguments: (argument_list . (identifier)? @first . (identifier)* @rest .))",
        "(_ !declarator) @node",
        "(identifier) @first (number_literal) @second (comment) @third",
        "(argument_list . (identifier)? @first . (number_literal)? @number . (identifier)* @rest .)",
    ] {
        let mut query = Query::new(&language, pattern).unwrap();
        let mut reference_query = tree_squatter::Query::new(&language, pattern).unwrap();
        let mut cursor = QueryCursor::new();
        let mut reference_cursor = tree_squatter::QueryCursor::new();
        cursor.set_optimized(true);
        reference_cursor.set_optimized(false);

        assert_eq!(
            matches!(&mut cursor, &query, tree, source),
            matches!(&mut reference_cursor, &reference_query, reference, source),
            "{pattern}"
        );
        assert_eq!(
            captures!(&mut cursor, &query, tree, source),
            captures!(&mut reference_cursor, &reference_query, reference, source),
            "{pattern}"
        );

        let capture = query.capture_names()[0].clone();
        query.disable_capture(&capture);
        reference_query.disable_capture(&capture);
        assert_eq!(
            matches!(&mut cursor, &query, tree, source),
            matches!(&mut reference_cursor, &reference_query, reference, source),
            "disabled capture: {pattern}"
        );
        query.disable_pattern(0);
        reference_query.disable_pattern(0);
        assert_eq!(
            matches!(&mut cursor, &query, tree, source),
            matches!(&mut reference_cursor, &reference_query, reference, source),
            "disabled pattern: {pattern}"
        );
    }
}

#[test]
fn disabling_non_rooted_pattern_enables_ranges() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) };
    let grammar = Grammar::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let source = "int value = 1;";
    let native = parser.parse(source, None).unwrap();
    let tree = Tree::pack(&grammar, &native).unwrap();
    let pattern = "((identifier) @name (number_literal) @value)\n(identifier) @other";

    let identifier = tree
        .root_node()
        .preorder()
        .nodes()
        .find(|node| node.kind() == "identifier")
        .unwrap();
    for optimized in [false, true] {
        let mut query = Query::new(&language, pattern).unwrap();
        let mut cursor = QueryCursor::new();
        cursor.set_optimized(optimized);
        assert!(cursor.set_byte_range(4..9));

        let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
        assert!(execution.next_match().is_none());
        assert_eq!(
            execution.error(),
            Some(tree_squatter::QueryExecutionError::UnsupportedRange)
        );
        drop(execution);

        // Removing the only non-rooted entry changes range eligibility, even
        // though its compiled steps and the other plans remain in storage.
        for _ in 0..2 {
            query.disable_pattern(0);
            assert_eq!(
                matches!(&mut cursor, &query, tree, source),
                vec![(1, vec![(identifier.slot().get(), 2)])],
            );
        }
    }
}

#[test]
fn cancellation_limits_ranges_and_reuse() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) };
    let grammar = Grammar::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let source = "int f() { return call(a, b, c, d, e, f, g, h); }\n".repeat(1000);
    let tree = Tree::parse(&grammar, &mut parser, &source).unwrap();
    let query = Query::new(
        &language,
        "(argument_list (identifier)* @before (identifier)* @after)",
    )
    .unwrap();
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(2);
    let results = matches!(&mut cursor, &query, tree, source);
    assert!(cursor.did_exceed_match_limit());
    assert!(!results.is_empty());

    cursor.set_timeout(Some(Duration::ZERO));
    let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
    while execution.next_match().is_some() {}
    assert!(execution.did_cancel());
    drop(execution);

    cursor.set_timeout(None);
    cursor.set_match_limit(u32::MAX);
    assert!(cursor.set_byte_range(1..10));
    let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
    assert!(execution.next_match().is_none());
    assert_eq!(
        execution.error(),
        Some(tree_squatter::QueryExecutionError::UnsupportedRange)
    );
    drop(execution);

    let query = Query::new(&language, "(identifier) @name").unwrap();
    let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
    let first = execution.next_capture().unwrap().0.id;
    execution.remove_match(first);
    while let Some((result, _)) = execution.next_capture() {
        assert_ne!(result.id, first);
    }
    assert!(execution.error().is_none());
}

#[test]
fn switching_between_matches_and_captures_preserves_finished_order() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) };
    let source = "int alpha(int beta) { return gamma(beta); }\n".repeat(20);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse(&source, None).unwrap();
    let tree = Tree::pack(&Grammar::new(&language).unwrap(), &native).unwrap();
    let reference =
        tree_squatter::Tree::pack(&tree_squatter::Grammar::new(&language).unwrap(), &native)
            .unwrap();
    let pattern = "(identifier) @first (identifier) @second (identifier) @third";
    let query = Query::new(&language, pattern).unwrap();
    let reference_query = tree_squatter::Query::new(&language, pattern).unwrap();

    macro_rules! record {
        ($result:expr) => {
            $result.map(|result| {
                (
                    result.pattern_index,
                    result
                        .captures
                        .iter()
                        .map(|capture| (u32::from(capture.node.slot()), capture.index))
                        .collect::<Vec<_>>(),
                )
            })
        };
    }

    for optimized in [false, true] {
        let mut cursor = QueryCursor::new();
        let mut reference_cursor = tree_squatter::QueryCursor::new();
        cursor.set_optimized(optimized);
        reference_cursor.set_optimized(false);
        let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
        let mut reference_execution =
            reference_cursor.execute(&reference_query, reference.root_node(), source.as_bytes());

        // Multiple finished patterns per node exercise both an untouched queue
        // and a partially consumed heap when the caller changes stream type.
        for index in 0..source.len() {
            let (actual, expected) = if index % 3 == 0 {
                (
                    record!(execution.next_capture().map(|(result, _)| result)),
                    record!(reference_execution.next_capture().map(|(result, _)| result)),
                )
            } else {
                (
                    record!(execution.next_match()),
                    record!(reference_execution.next_match()),
                )
            };
            assert_eq!(actual, expected, "operation {index}, optimized={optimized}");
            if actual.is_none() {
                break;
            }
        }
        assert!(execution.next_match().is_none());
        assert!(execution.error().is_none());
    }
}

#[test]
fn query_edge_cases_match_tree_sitter() {
    use std::collections::BTreeSet;
    use tree_sitter::{Point, StreamingIterator};
    for language in [tree_sitter_json::LANGUAGE, tree_sitter_c::LANGUAGE] {
        let language = unsafe { tree_sitter::Language::from_raw(language.into_raw()().cast()) };
        let grammar = Grammar::new(&language).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let mut patterns = vec![
            "(_) @node".to_owned(),
            "(_) @one (_) @two".to_owned(),
            "[(ERROR) (_)] @node".to_owned(),
            "(MISSING) @missing".to_owned(),
            "(_ (_) @child) @parent".to_owned(),
            "(_ . (_) @first)".to_owned(),
            "(_ (_) @last .)".to_owned(),
            "(_ (_) @first . (_) @second)".to_owned(),
            "(_ . (_) @first . (_) @second)".to_owned(),
            "(_ (_)+ @children) @parent".to_owned(),
            "(_ (_)* @children) @parent".to_owned(),
            "(_ (_)? @child . (_) @last)".to_owned(),
            "(object (pair key: (string) @key value: (string) @value)) @object".to_owned(),
            "[(_) (_)] @alternative".to_owned(),
            "(not_a_real_symbol) @invalid".to_owned(),
            "(_".to_owned(),
            "(_) @".to_owned(),
        ];
        for field in 1..=language.field_count() as u16 {
            let name = language.field_name_for_id(field).unwrap();
            patterns.push(format!("(_ {name}: (_) @child) @parent"));
            patterns.push(format!("(_ !{name}) @parent"));
        }
        for &symbol in language.supertypes() {
            patterns.push(format!(
                "({}) @supertype",
                language.node_kind_for_id(symbol).unwrap()
            ));
        }
        let roots = (0..language.node_kind_count() as u16)
            .filter(|&symbol| {
                language.node_kind_is_named(symbol) && language.node_kind_is_visible(symbol)
            })
            .take(12)
            .map(|symbol| format!("({})", language.node_kind_for_id(symbol).unwrap()))
            .collect::<Vec<_>>()
            .join(" ");
        patterns.push(format!("[{roots}] @roots"));
        for source in [
            "",
            "x",
            "{\"x\": [1, 2, 3], \"y\": true}",
            "[{\"x\":1},{\"x\":2},{\"x\":\"yes\"}]",
            "{\"x\": [1,",
            "int f(int x) { return x + 1; }",
            "// comment\n x = 1\n",
        ] {
            let native = parser.parse(source, None).unwrap();
            let tree = Tree::pack(&grammar, &native).unwrap();
            for pattern in &patterns {
                let expected = tree_sitter::Query::new(&language, pattern);
                let actual = Query::new(&language, pattern);
                let (expected, actual) = match (expected, actual) {
                    (Ok(expected), Ok(actual)) => (expected, actual),
                    (Err(expected), Err(actual)) => {
                        assert_eq!(expected.offset, actual.offset, "{pattern}");
                        continue;
                    }
                    _ => panic!("compilation differs: {pattern}"),
                };
                for optimized in [false, true] {
                    for mode in 0..4 {
                        let mut reference = tree_sitter::QueryCursor::new();
                        let mut cursor = QueryCursor::new();
                        cursor.set_optimized(optimized);
                        match mode {
                            1 => {
                                reference.set_max_start_depth(Some(1));
                                cursor.set_max_start_depth(1);
                            }
                            2 => {
                                reference.set_byte_range(1..12);
                                assert!(cursor.set_byte_range(1..12));
                            }
                            3 => {
                                reference.set_point_range(Point::new(0, 1)..Point::new(1, 0));
                                assert!(cursor.set_point_range(Point::new(0, 1)..Point::new(1, 0)));
                            }
                            _ => {}
                        }
                        let mut execution =
                            reference.matches(&expected, native.root_node(), source.as_bytes());
                        let mut expected_matches = Vec::new();
                        while let Some(result) = execution.next() {
                            expected_matches.push((
                                result.pattern_index,
                                result
                                    .captures()
                                    .iter()
                                    .map(|capture| {
                                        (
                                            capture.node.start_byte(),
                                            capture.node.end_byte(),
                                            capture.node.kind_id(),
                                            capture.index,
                                        )
                                    })
                                    .collect::<Vec<_>>(),
                            ));
                        }
                        let mut execution =
                            cursor.execute(&actual, tree.root_node(), source.as_bytes());
                        let mut actual_matches = Vec::new();
                        while let Some(result) = execution.next_match() {
                            actual_matches.push((
                                result.pattern_index,
                                result
                                    .captures
                                    .iter()
                                    .map(|capture| {
                                        (
                                            capture.node.start_byte(),
                                            capture.node.end_byte(),
                                            capture.node.kind_id().get(),
                                            capture.index,
                                        )
                                    })
                                    .collect::<Vec<_>>(),
                            ));
                            assert!(actual_matches.len() < 100_000);
                        }
                        if execution.error()
                            == Some(tree_squatter::QueryExecutionError::UnsupportedRange)
                        {
                            assert!(mode >= 2 && actual_matches.is_empty());
                            continue;
                        }
                        assert!(execution.error().is_none());
                        expected_matches.sort();
                        actual_matches.sort();
                        assert_eq!(
                            expected_matches, actual_matches,
                            "{pattern}, {source:?}, mode={mode}, optimized={optimized}"
                        );
                        drop(execution);
                        let mut execution =
                            cursor.execute(&actual, tree.root_node(), source.as_bytes());
                        let mut captured = BTreeSet::new();
                        let mut events = 0;
                        while let Some((result, index)) = execution.next_capture() {
                            let capture = result.captures[index];
                            captured.insert((
                                result.pattern_index,
                                (
                                    capture.node.start_byte(),
                                    capture.node.end_byte(),
                                    capture.node.kind_id().get(),
                                    capture.index,
                                ),
                            ));
                            events += 1;
                            assert!(events < 100_000);
                        }
                        assert!(execution.error().is_none());
                        if mode < 2 {
                            for (pattern, captures) in expected_matches {
                                for capture in captures {
                                    if capture.1 > 0 {
                                        assert!(
                                            captured.contains(&(pattern, capture)),
                                            "pattern {pattern}, capture {capture:?}, source {source:?}, mode={mode}, optimized={optimized}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn disabled_rootless_and_branching_range_eligibility() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Grammar::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let tree = Tree::parse(&grammar, &mut parser, "[1,2,3]").unwrap();
    for (index, pattern) in [
        "(_) @first\n(_) @node",
        "((_) @first (_) @second)\n(_) @node",
        "[(_) (_)] @first\n(_) @node",
    ]
    .into_iter()
    .enumerate()
    {
        let mut query = Query::new(&language, pattern).unwrap();
        for disabled in [false, true] {
            if disabled {
                query.disable_pattern(0);
                query.disable_pattern(0);
            }
            query.disable_capture("first");
            for optimized in [false, true] {
                for points in [false, true] {
                    let mut cursor = QueryCursor::new();
                    cursor.set_optimized(optimized);
                    if points {
                        assert!(cursor.set_point_range(
                            tree_sitter::Point::new(0, 1)..tree_sitter::Point::new(1, 0)
                        ));
                    } else {
                        assert!(cursor.set_byte_range(1..12));
                    }
                    for captures in [false, true] {
                        let mut execution = cursor.execute(&query, tree.root_node(), b"[1,2,3]");
                        let found = if captures {
                            execution.next_capture().is_some()
                        } else {
                            execution.next_match().is_some()
                        };
                        let supported = index == 0 || (index == 1 && disabled);
                        assert!(supported || !found);
                        assert_eq!(
                            execution.error(),
                            if supported {
                                None
                            } else {
                                Some(tree_squatter::QueryExecutionError::UnsupportedRange)
                            }
                        );
                    }
                }
            }
        }
    }
}
