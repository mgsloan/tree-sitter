use std::time::Duration;
use tree_squatter_rust::{Grammar, Query, QueryCursor, Tree};

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
fn queries_match_reference_with_and_without_plans() {
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
        for optimized in [false, true] {
            let mut query = Query::new(&language, pattern).unwrap();
            let mut reference_query = tree_squatter::Query::new(&language, pattern).unwrap();
            let mut cursor = QueryCursor::new();
            let mut reference_cursor = tree_squatter::QueryCursor::new();
            cursor.set_optimized(optimized);
            reference_cursor.set_optimized(optimized);

            assert_eq!(
                matches!(&mut cursor, &query, tree, source),
                matches!(&mut reference_cursor, &reference_query, reference, source),
                "{pattern}, optimized={optimized}"
            );
            assert_eq!(
                captures!(&mut cursor, &query, tree, source),
                captures!(&mut reference_cursor, &reference_query, reference, source),
                "{pattern}, optimized={optimized}"
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
}

#[test]
fn disabling_non_rooted_pattern_enables_ranges() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) };
    let grammar = Grammar::new(&language).unwrap();
    let reference_grammar = tree_squatter::Grammar::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let source = "int value = 1;";
    let native = parser.parse(source, None).unwrap();
    let tree = Tree::pack(&grammar, &native).unwrap();
    let reference = tree_squatter::Tree::pack(&reference_grammar, &native).unwrap();
    let pattern = "((identifier) @name (number_literal) @value)\n(identifier) @other";

    for optimized in [false, true] {
        let mut query = Query::new(&language, pattern).unwrap();
        let mut reference_query = tree_squatter::Query::new(&language, pattern).unwrap();
        let mut cursor = QueryCursor::new();
        let mut reference_cursor = tree_squatter::QueryCursor::new();
        cursor.set_optimized(optimized);
        reference_cursor.set_optimized(optimized);
        assert!(cursor.set_byte_range(4..9));
        assert!(reference_cursor.set_byte_range(4..9));

        let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
        assert!(execution.next_match().is_none());
        assert_eq!(
            execution.error(),
            Some(tree_squatter_rust::QueryExecutionError::UnsupportedRange)
        );
        drop(execution);

        // Removing the only non-rooted entry changes range eligibility, even
        // though its compiled steps and the other plans remain in storage.
        for _ in 0..2 {
            query.disable_pattern(0);
            reference_query.disable_pattern(0);
            assert_eq!(
                matches!(&mut cursor, &query, tree, source),
                matches!(&mut reference_cursor, &reference_query, reference, source),
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
        Some(tree_squatter_rust::QueryExecutionError::UnsupportedRange)
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
        reference_cursor.set_optimized(optimized);
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
