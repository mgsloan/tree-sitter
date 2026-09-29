mod support;

use support::{
    QueryResult as QuerySnapshot, capture_set, native_query_results_with_cursor, query_results,
    query_snapshot as snapshot,
};
use support::{c_language, json_language};

use std::ops::ControlFlow;
use tree_squatter::QueryCursorOptions;
use tree_squatter::{Forest, Language, Query, QueryCursor};

macro_rules! matches {
    ($cursor:expr, $query:expr, $tree:expr, $source:expr) => {{
        let mut execution = $cursor.execute($query, $tree.root_node(), $source.as_bytes());
        let mut results = Vec::new();
        while let Some(result) = execution.next_match() {
            results.push((
                result.pattern_index.raw(),
                result
                    .captures()
                    .iter()
                    .map(|capture| (u32::from(capture.node.slot()), capture.index.raw()))
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
            let capture = result.captures()[index.raw() as usize];
            results.push((
                result.pattern_index.raw(),
                u32::from(capture.node.slot()),
                capture.index.raw(),
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
    let language = c_language();
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    // Three ambiguous capture runs create enough states to exercise indexed deduplication.
    let source = "// before\nint alpha(int x, int y) { int a = 1; if (x) return beta(x, y, a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t, u, v, w, x, y, z); return y; }\n// after\nint bravo = 2;\nint other() { return beta(a, b); }";
    let native = parser.parse(source, None).unwrap();
    let tree = Forest::pack(&grammar, &native).unwrap();

    for pattern in [
        "(identifier) @identifier",
        "(_) @node",
        "[ (identifier) (number_literal) ] @value",
        "[ (identifier) (number_literal) (comment) (return_statement) ] @value",
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
        let mut query = Query::new(&grammar, pattern).unwrap();
        let mut reference_query = tree_squatter::Query::new(&grammar, pattern).unwrap();
        let mut cursor = QueryCursor::new();
        let mut reference_cursor = tree_squatter::QueryCursor::new();
        cursor.set_optimized(true);
        reference_cursor.set_optimized(false);

        assert_eq!(
            matches!(&mut cursor, &query, tree, source),
            matches!(&mut reference_cursor, &reference_query, tree, source),
            "{pattern}"
        );
        assert_eq!(
            captures!(&mut cursor, &query, tree, source),
            captures!(&mut reference_cursor, &reference_query, tree, source),
            "{pattern}"
        );

        let capture = query.capture_names()[0].to_owned();
        query.disable_capture(&capture);
        reference_query.disable_capture(&capture);
        assert_eq!(
            matches!(&mut cursor, &query, tree, source),
            matches!(&mut reference_cursor, &reference_query, tree, source),
            "disabled capture: {pattern}"
        );
        query.disable_pattern(tree_squatter::PatternIx(0));
        reference_query.disable_pattern(tree_squatter::PatternIx(0));
        assert_eq!(
            matches!(&mut cursor, &query, tree, source),
            matches!(&mut reference_cursor, &reference_query, tree, source),
            "disabled pattern: {pattern}"
        );
    }
}

#[test]
fn error_queries_survive_native_mutations() {
    for (language, source) in [
        (tree_sitter_json::LANGUAGE, "[1, ?, 2]"),
        (
            tree_sitter_c_sharp::LANGUAGE,
            "class C { int M() { ? return 1; } }",
        ),
    ] {
        let language = unsafe { tree_sitter::Language::from_raw(language.into_raw()().cast()) };
        let grammar = Language::new(&language).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let native = parser.parse(source, None).unwrap();
        let tree = Forest::pack(&grammar, &native).unwrap();
        assert!(tree.root_node().has_error());

        for pattern in [
            "(ERROR) @error (_) @node",
            "(ERROR) @error (_ (_) @child) @parent",
        ] {
            let mut query = Query::new(&grammar, pattern).unwrap();
            let reference = tree_sitter::Query::new(&language, pattern).unwrap();
            let error_capture = reference.capture_index_for_name("error").unwrap();
            let mut reference_cursor = tree_sitter::QueryCursor::new();
            let mut expected = native_query_results_with_cursor(
                &mut reference_cursor,
                &reference,
                native.root_node(),
                source.as_bytes(),
                false,
            );
            assert!(expected.iter().any(|(pattern, _, _)| *pattern == 0));

            for mutation in 0..4 {
                match mutation {
                    1 => {
                        query.disable_capture("error");
                        for (_, _, captures) in &mut expected {
                            captures.retain(|(index, _)| *index != error_capture);
                        }
                    }
                    2 | 3 => {
                        let pattern = 3 - mutation;
                        query.disable_pattern(tree_squatter::PatternIx(pattern));
                        expected.retain(|(index, _, _)| *index != pattern);
                    }
                    _ => {}
                }

                for optimized in [false, true] {
                    let mut cursor = QueryCursor::new();
                    cursor.set_optimized(optimized);
                    let actual = query_results(
                        &mut cursor,
                        &query,
                        tree.root_node(),
                        source.as_bytes(),
                        false,
                    );
                    assert_eq!(
                        actual, expected,
                        "{pattern}, {source:?}, mutation={mutation}, optimized={optimized}"
                    );
                }
            }
        }
    }
}

#[test]
fn malformed_queries_match_with_and_without_plans() {
    use std::collections::BTreeSet;

    for (name, language, source) in [
        (
            "json",
            tree_sitter_json::LANGUAGE,
            "{\"key\": [1,2,3], \"nested\": {\"value\":\"text\"}}",
        ),
        (
            "c",
            tree_sitter_c::LANGUAGE,
            "int f(int x) { int y = 1; return x + y; }",
        ),
        (
            "c_sharp",
            tree_sitter_c_sharp::LANGUAGE,
            "class C { int M(int x) { return x + 1; } }",
        ),
    ] {
        let language = unsafe { tree_sitter::Language::from_raw(language.into_raw()().cast()) };
        let grammar = Language::new(&language).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let native = parser.parse(source, None).unwrap();
        assert!(!native.root_node().has_error());
        let tree = Forest::pack(&grammar, &native).unwrap();
        let mut patterns = [
            "(ERROR) @error",
            "(MISSING) @missing",
            "(_) @node",
            "(_ . (_) @first)",
            "(_ . (_) @first . (_) @second .)",
            "(ERROR . (_) @first)",
            "(ERROR (_) @child) @error",
            "(_ (ERROR) @error) @parent",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
        for parent in tree
            .root_node()
            .preorder()
            .nodes()
            .filter(|node| node.is_named())
        {
            for child in parent.named_children(&mut parent.walk()) {
                patterns.insert(format!(
                    "({} ({}) @child) @parent",
                    parent.kind(),
                    child.kind()
                ));
                patterns.insert(format!("({} . ({}) @child)", parent.kind(), child.kind()));
                if let Some(field) = child.field_id() {
                    let field = language.field_name_for_id(field.raw()).unwrap();
                    patterns.insert(format!(
                        "({} {field}: ({}) @child) @parent",
                        parent.kind(),
                        child.kind()
                    ));
                }
                for descendant in child.named_children(&mut child.walk()) {
                    patterns.insert(format!(
                        "({} ({} ({}) @descendant)) @parent",
                        parent.kind(),
                        child.kind(),
                        descendant.kind()
                    ));
                }
            }
        }
        let queries = patterns
            .into_iter()
            .filter_map(|pattern| {
                Query::new(&grammar, &pattern)
                    .ok()
                    .map(|query| (pattern, query))
            })
            .collect::<Vec<_>>();
        let mut sources = BTreeSet::new();
        for offset in 0..=source.len() {
            sources.insert(source[..offset].to_owned());
            let mut inserted = source.to_owned();
            inserted.insert(offset, '?');
            sources.insert(inserted);
            if offset < source.len() {
                let mut deleted = source.to_owned();
                deleted.remove(offset);
                sources.insert(deleted);
            }
        }
        let mut errors = 0;
        for source in sources {
            let native = parser.parse(&source, None).unwrap();
            if !native.root_node().has_error() {
                continue;
            }
            errors += 1;
            let tree = Forest::pack(&grammar, &native).unwrap();
            let capturable_slots = tree
                .root_node()
                .preorder()
                .nodes()
                .filter(|node| node.end_byte() > 0)
                .map(|node| node.slot().raw())
                .collect::<BTreeSet<_>>();
            for (pattern, query) in &queries {
                for bounded in [false, true] {
                    let mut optimized = QueryCursor::new();
                    let mut reference = QueryCursor::new();
                    reference.set_optimized(false);
                    if bounded {
                        let start = source.len() / 2;
                        optimized.set_byte_range(start..start + 1);
                        reference.set_byte_range(start..start + 1);
                    }
                    let expected = matches!(&mut reference, query, tree, source);
                    assert_eq!(
                        matches!(&mut optimized, query, tree, source),
                        expected,
                        "{name}, {pattern}, {source:?}, bounded={bounded}"
                    );
                    if !bounded {
                        let actual = captures!(&mut optimized, query, tree, source);
                        for (pattern_index, captures) in expected {
                            for (slot, index) in captures {
                                if capturable_slots.contains(&slot) {
                                    assert!(
                                        actual.contains(&(pattern_index, slot, index)),
                                        "capture {index}, {name}, {pattern}, {source:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(errors > 50, "{name}: {errors}");
        eprintln!(
            "{name}: {errors} malformed inputs, {} queries, {} match comparisons",
            queries.len(),
            errors * queries.len() * 2
        );
    }
}

#[test]
fn presence_scans_across_groups() {
    let language = json_language();
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();

    for count in [1, 7, 31, 32, 33, 63, 64, 65, 255, 256, 257] {
        for errors in [false, true] {
            let source = format!(
                "[{}]",
                (0..count)
                    .map(|index| if errors && index % 5 == 0 {
                        "{\"a\":[?]}"
                    } else {
                        match index % 3 {
                            0 => "{\"a\":[0]}",
                            1 => "{\"a\":[false]}",
                            _ => "{\"a\":[true]}",
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let tree = Forest::parse(&grammar, &mut parser, &source).unwrap();
            assert_eq!(tree.root_node().has_error(), errors);
            for pattern in [
                "(object (pair value: (array (false) @value)))",
                "(object (pair value: (array (null) @value)))",
                "(array (object (pair value: (array (true) @value))))",
            ] {
                let query = Query::new(&grammar, pattern).unwrap();
                let mut optimized = QueryCursor::new();
                let mut reference = QueryCursor::new();
                reference.set_optimized(false);
                assert_eq!(
                    matches!(&mut optimized, &query, tree, source),
                    matches!(&mut reference, &query, tree, source),
                    "count={count}, errors={errors}, {pattern}"
                );
            }
        }
    }
}

#[test]
fn disabling_non_rooted_pattern_preserves_ranges() {
    let source = "int value = 1;";
    let (grammar, tree) = query_tree(c_language(), source);
    let pattern = "((identifier) @name (number_literal) @value)\n(identifier) @other";

    let identifier = tree
        .root_node()
        .preorder()
        .nodes()
        .find(|node| node.kind() == "identifier")
        .unwrap();
    let number = tree
        .root_node()
        .preorder()
        .nodes()
        .find(|node| node.kind() == "number_literal")
        .unwrap();
    for optimized in [false, true] {
        let mut query = Query::new(&grammar, pattern).unwrap();
        let mut cursor = QueryCursor::new();
        cursor.set_optimized(optimized);
        cursor.set_byte_range(4..9);

        assert_eq!(
            matches!(&mut cursor, &query, tree, source),
            vec![
                (
                    0,
                    vec![(identifier.slot().raw(), 0), (number.slot().raw(), 1)]
                ),
                (1, vec![(identifier.slot().raw(), 2)]),
            ],
        );

        for _ in 0..2 {
            query.disable_pattern(tree_squatter::PatternIx(0));
            assert_eq!(
                matches!(&mut cursor, &query, tree, source),
                vec![(1, vec![(identifier.slot().raw(), 2)])],
            );
        }
    }
}

#[test]
fn cancellation_with_match_limit() {
    let language = c_language();
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let source = "int f() { return call(a, b, c, d, e, f, g, h); }\n".repeat(1000);
    let tree = Forest::parse(&grammar, &mut parser, &source).unwrap();
    let query = Query::new(
        &grammar,
        "(argument_list (identifier)* @before (identifier)* @after)",
    )
    .unwrap();
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(2);
    let results = matches!(&mut cursor, &query, tree, source);
    assert!(cursor.did_exceed_match_limit());
    assert!(!results.is_empty());

    let mut stop = |_: &tree_squatter::QueryCursorState| ControlFlow::Break(());
    let mut execution = cursor.execute_with_options(
        &query,
        tree.root_node(),
        source.as_bytes(),
        QueryCursorOptions::new().progress_callback(&mut stop),
    );
    let mut completed_matches = 0;
    while execution.next_match().is_some() {
        completed_matches += 1;
    }
    assert!(completed_matches < results.len());
    assert_eq!(execution.error(), None);
    drop(execution);
}

#[test]
fn switching_between_matches_and_captures_preserves_finished_order() {
    let language = c_language();
    let source = "int alpha(int beta) { return gamma(beta); }\n".repeat(20);
    let native = support::parse_native(&language, &source);
    let grammar = Language::new(&language).unwrap();
    let tree = Forest::pack(&grammar, &native).unwrap();
    let pattern = "(identifier) @first (identifier) @second (identifier) @third";
    let query = Query::new(&grammar, pattern).unwrap();

    macro_rules! record {
        ($result:expr) => {
            $result.map(|result| {
                (
                    result.pattern_index,
                    result
                        .captures()
                        .iter()
                        .map(|capture| (u32::from(capture.node.slot()), capture.index))
                        .collect::<Vec<_>>(),
                )
            })
        };
    }

    let mut cursor = QueryCursor::new();
    let mut reference_cursor = tree_squatter::QueryCursor::new();
    cursor.set_optimized(true);
    reference_cursor.set_optimized(false);
    let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
    let mut reference_execution =
        reference_cursor.execute(&query, tree.root_node(), source.as_bytes());

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
        assert_eq!(actual, expected, "operation {index}");
        if actual.is_none() {
            break;
        }
    }
    assert!(execution.next_match().is_none());
    assert!(execution.error().is_none());
}

#[test]
fn query_edge_cases_match_tree_sitter() {
    use tree_sitter::Point;
    for language in [tree_sitter_json::LANGUAGE, tree_sitter_c::LANGUAGE] {
        let language = unsafe { tree_sitter::Language::from_raw(language.into_raw()().cast()) };
        let grammar = Language::new(&language).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let mut patterns = vec![
            "(ERROR) @error".to_owned(),
            "(_) @node".to_owned(),
            "(_) @one (_) @two".to_owned(),
            "((_) @first (_) @second)".to_owned(),
            "((_) @first . (_) @second)".to_owned(),
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
            "[(number) (true)] @value".to_owned(),
            "(array [(number) (true)]* @values)".to_owned(),
            "(array (number)? @first (number)* @rest)".to_owned(),
            "(array . (number) @first . (number) @second . (number) @third .) @array".to_owned(),
            "((number) @first (number) @second)".to_owned(),
            "(\"[\" @open \"]\" @close) (\"{\" @open \"}\" @close)".to_owned(),
            "(array (number))".to_owned(),
            "(array (number) @number)".to_owned(),
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
            "[\n  1, 2,\n  3, true,\n  [4, 5]\n]\n",
        ] {
            let native = parser.parse(source, None).unwrap();
            let tree = Forest::pack(&grammar, &native).unwrap();
            for pattern in &patterns {
                let expected = tree_sitter::Query::new(&language, pattern);
                let actual = Query::new(&grammar, pattern);
                let (expected, actual) = match (expected, actual) {
                    (Ok(expected), Ok(actual)) => (expected, actual),
                    (Err(expected), Err(actual)) => {
                        assert_eq!(expected.offset, actual.offset, "{pattern}");
                        continue;
                    }
                    _ => panic!("compilation differs: {pattern}"),
                };
                for optimized in [false, true] {
                    for mode in 0..15 {
                        let mut reference = tree_sitter::QueryCursor::new();
                        let mut cursor = QueryCursor::new();
                        cursor.set_optimized(optimized);
                        macro_rules! configure {
                            ($cursor:ident) => {
                                match mode {
                                    1 => {
                                        $cursor.set_max_start_depth(Some(1));
                                    }
                                    2 => {
                                        $cursor.set_byte_range(1..12);
                                    }
                                    3 => {
                                        $cursor.set_point_range(Point::new(0, 1)..Point::new(1, 0));
                                    }
                                    4 => {
                                        $cursor.set_byte_range(4..4);
                                    }
                                    5 => {
                                        $cursor.set_point_range(Point::new(1, 2)..Point::new(1, 3));
                                    }
                                    6 => {
                                        $cursor.set_byte_range(1..12);
                                        $cursor.set_point_range(Point::new(1, 0)..Point::new(2, 0));
                                    }
                                    7 => {
                                        $cursor.set_byte_range(100..101);
                                    }
                                    8 => {
                                        $cursor.set_containing_byte_range(1..12);
                                    }
                                    9 => {
                                        $cursor.set_containing_point_range(
                                            Point::new(0, 1)..Point::new(1, 0),
                                        );
                                    }
                                    10 => {
                                        $cursor.set_containing_byte_range(4..4);
                                    }
                                    11 => {
                                        $cursor
                                            .set_containing_byte_range(1..12)
                                            .set_containing_point_range(
                                                Point::new(1, 0)..Point::new(2, 0),
                                            );
                                    }
                                    12 => {
                                        $cursor.set_containing_byte_range(100..101);
                                    }
                                    13 => {
                                        $cursor.set_containing_byte_range(1..source.len());
                                    }
                                    14 => {
                                        $cursor
                                            .set_containing_point_range(
                                                Point::new(1, 2)..Point::new(1, 2),
                                            )
                                            .set_byte_range(1..12);
                                    }
                                    _ => {}
                                }
                            };
                        }
                        configure!(reference);
                        configure!(cursor);
                        let mut expected_matches = native_query_results_with_cursor(
                            &mut reference,
                            &expected,
                            native.root_node(),
                            source.as_bytes(),
                            false,
                        );
                        let mut actual_matches = query_results(
                            &mut cursor,
                            &actual,
                            tree.root_node(),
                            source.as_bytes(),
                            false,
                        );
                        expected_matches.sort();
                        actual_matches.sort();
                        assert_eq!(
                            expected_matches, actual_matches,
                            "{pattern}, {source:?}, mode={mode}, optimized={optimized}"
                        );
                        let captured = capture_set(query_results(
                            &mut cursor,
                            &actual,
                            tree.root_node(),
                            source.as_bytes(),
                            true,
                        ));
                        if mode >= 2 {
                            let expected_captures = capture_set(native_query_results_with_cursor(
                                &mut reference,
                                &expected,
                                native.root_node(),
                                source.as_bytes(),
                                true,
                            ));
                            assert_eq!(
                                expected_captures, captured,
                                "{pattern}, {source:?}, mode={mode}, optimized={optimized}"
                            );
                        } else {
                            for (pattern, _, captures) in expected_matches {
                                for capture in captures {
                                    if capture.1.2 > 0 {
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
fn containing_ranges_combine_with_intersecting_ranges() {
    use tree_sitter::{Point, StreamingIterator};

    let source = "[\n [10,20],\n [30,40],\n [50,60]\n]";
    let (grammar, tree) = json_query_tree(source);
    let start = source.find("[30").unwrap();
    let end = start + "[30,40]".len();
    for pattern in [
        "(array . (number) @first . (number) @last .)",
        "((number) @first (number) @last)",
    ] {
        let query = Query::new(&grammar, pattern).unwrap();
        for optimized in [false, true] {
            let mut cursor = QueryCursor::new();
            cursor.set_optimized(optimized);
            cursor
                .set_containing_byte_range(0..end)
                .set_containing_point_range(Point::new(2, 0)..Point::new(0, 0))
                .set_byte_range(start + 1..start + 3)
                .set_point_range(Point::new(2, 2)..Point::new(2, 4));
            {
                let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
                let found = matches.next().unwrap();
                assert_eq!(
                    found
                        .captures()
                        .iter()
                        .map(|capture| &source[capture.node.byte_range()])
                        .collect::<Vec<_>>(),
                    ["30", "40"],
                    "{pattern}, optimized={optimized}"
                );
                assert!(matches.next().is_none());
            }
            {
                let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
                let (found, index) = captures.next().unwrap();
                assert_eq!(
                    &source[found.captures()[index.raw() as usize].node.byte_range()],
                    "30"
                );
                assert!(captures.next().is_none());
            }

            // The uncaptured array root must also fit inside the containing range.
            let rooted = Query::new(&grammar, "(array (number) @number)").unwrap();
            cursor.set_containing_byte_range(start + 1..end - 1);
            assert!(
                cursor
                    .matches(&rooted, tree.root_node(), source.as_bytes())
                    .next()
                    .is_none()
            );
            cursor
                .set_containing_byte_range(0..0)
                .set_containing_point_range(Point::new(0, 0)..Point::new(0, 0))
                .set_byte_range(0..0)
                .set_point_range(Point::new(0, 0)..Point::new(0, 0));
            assert_eq!(
                cursor
                    .matches(&rooted, tree.root_node(), source.as_bytes())
                    .count(),
                6
            );
        }
    }
}

#[test]
fn containing_ranges_include_missing_nodes_at_the_end() {
    use tree_sitter::{Point, StreamingIterator};

    let language = c_language();
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let source = "int f() { return 1 }";
    let native = parser.parse(source, None).unwrap();
    let tree = Forest::pack(&grammar, &native).unwrap();
    let missing = tree
        .root_node()
        .preorder()
        .nodes()
        .find(|node| node.is_missing())
        .unwrap();
    let end = missing.end_byte();
    let query = Query::new(&grammar, "\";\" @semicolon").unwrap();
    let reference_query = tree_sitter::Query::new(&language, "\";\" @semicolon").unwrap();
    for optimized in [false, true] {
        for points in [false, true] {
            for range in [0..end, end..end, end..0, end + 1..0] {
                let mut cursor = QueryCursor::new();
                let mut reference = tree_sitter::QueryCursor::new();
                cursor.set_optimized(optimized);
                if points {
                    let range = Point::new(0, range.start)..Point::new(0, range.end);
                    cursor.set_containing_point_range(range.clone());
                    reference.set_containing_point_range(range);
                } else {
                    cursor.set_containing_byte_range(range.clone());
                    reference.set_containing_byte_range(range.clone());
                }
                let expected = reference
                    .matches(&reference_query, native.root_node(), source.as_bytes())
                    .count();
                let actual = cursor
                    .matches(&query, tree.root_node(), source.as_bytes())
                    .count();
                assert_eq!(
                    actual, expected,
                    "{range:?}, points={points}, optimized={optimized}"
                );
                assert_eq!(actual, usize::from(range.start == 0));
            }
        }
    }
}

#[test]
fn containing_ranges_finish_deferred_matches_in_error_subtrees() {
    use tree_squatter::StreamingIterator;

    let language = c_language();
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let source = "[\n  1, 2,\n  3, true,\n  [4, 5]\n]\n";
    let tree = Forest::parse(&grammar, &mut parser, source).unwrap();
    let query = Query::new(&grammar, "(_ (_)* @children) @parent").unwrap();
    for optimized in [false, true] {
        let mut cursor = QueryCursor::new();
        cursor.set_optimized(optimized);
        cursor.set_containing_byte_range(1..12).set_byte_range(4..5);
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        // Forest-sitter drops this deferred match when hidden traversal skips the
        // enclosing exit events. Squatter finishes it when exiting the parent.
        assert_eq!(
            matches.next().unwrap().captures()[0].node.byte_range(),
            4..5
        );
        assert!(matches.next().is_none());
    }
}

#[test]
fn quantified_roots_with_ranges_match_tree_sitter() {
    use tree_sitter::Point;

    let language = json_language();
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let source = "[1,2,3]";
    let native = parser.parse(source, None).unwrap();
    let tree = Forest::pack(&grammar, &native).unwrap();

    for pattern in [
        "(_)? @node",
        "(_)* @node",
        "(_)+ @node",
        "(number)? @node",
        "(number)* @node",
        "(number)+ @node",
        "((number)? @before (number) @after)",
    ] {
        let query = Query::new(&grammar, pattern).unwrap();
        let expected = tree_sitter::Query::new(&language, pattern).unwrap();
        for range in [0..0, 0..1, 1..2, 2..2, 3..4, 5..7, 7..8, 8..9] {
            for points in [false, true] {
                let mut reference = tree_sitter::QueryCursor::new();
                for optimized in [false, true] {
                    let mut cursor = QueryCursor::new();
                    cursor.set_optimized(optimized);
                    if points {
                        let range = Point::new(0, range.start)..Point::new(0, range.end);
                        reference.set_point_range(range.clone());
                        cursor.set_point_range(range);
                    } else {
                        reference.set_byte_range(range.clone());
                        cursor.set_byte_range(range.clone());
                    }

                    let mut expected_matches = native_query_results_with_cursor(
                        &mut reference,
                        &expected,
                        native.root_node(),
                        source.as_bytes(),
                        false,
                    );
                    let mut actual_matches = query_results(
                        &mut cursor,
                        &query,
                        tree.root_node(),
                        source.as_bytes(),
                        false,
                    );
                    // Hidden repetition nodes affect the number of empty matches.
                    expected_matches.retain(|(_, _, captures)| !captures.is_empty());
                    actual_matches.retain(|(_, _, captures)| !captures.is_empty());
                    expected_matches.sort();
                    actual_matches.sort();
                    assert_eq!(
                        expected_matches, actual_matches,
                        "{pattern}, {range:?}, points={points}, optimized={optimized}"
                    );
                    assert_eq!(
                        capture_set(query_results(
                            &mut cursor,
                            &query,
                            tree.root_node(),
                            source.as_bytes(),
                            true
                        )),
                        capture_set(native_query_results_with_cursor(
                            &mut reference,
                            &expected,
                            native.root_node(),
                            source.as_bytes(),
                            true
                        )),
                        "{pattern}, {range:?}, points={points}, optimized={optimized}",
                    );
                }
            }
        }
    }
}

#[test]
fn disabled_rootless_and_branching_patterns_with_ranges() {
    let (grammar, tree) = json_query_tree("[1,2,3]");
    for pattern in [
        "(_) @first\n(_) @node",
        "((_) @first (_) @second)\n(_) @node",
        "[(_) (_)] @first\n(_) @node",
    ] {
        let mut query = Query::new(&grammar, pattern).unwrap();
        for disabled in [false, true] {
            if disabled {
                query.disable_pattern(tree_squatter::PatternIx(0));
                query.disable_pattern(tree_squatter::PatternIx(0));
            }
            query.disable_capture("first");
            for optimized in [false, true] {
                for points in [false, true] {
                    let mut cursor = QueryCursor::new();
                    cursor.set_optimized(optimized);
                    if points {
                        cursor.set_point_range(
                            tree_sitter::Point::new(0, 1)..tree_sitter::Point::new(1, 0),
                        );
                    } else {
                        cursor.set_byte_range(1..12);
                    }
                    for captures in [false, true] {
                        let mut execution =
                            cursor.execute(&query, tree.root_node(), b"[1,2,3]".as_slice());
                        let mut found = false;
                        loop {
                            let result = if captures {
                                execution.next_capture().map(|(result, _)| result)
                            } else {
                                execution.next_match()
                            };
                            let Some(result) = result else { break };
                            assert!(!disabled || result.pattern_index.raw() != 0);
                            assert!(
                                result
                                    .captures()
                                    .iter()
                                    .all(|capture| capture.index.raw() != 0)
                            );
                            found = true;
                        }
                        assert!(found);
                        assert_eq!(execution.error(), None);
                    }
                }
            }
        }
    }
}

fn query_tree(language: tree_sitter::Language, source: &str) -> (Language, Forest) {
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let tree = Forest::parse(&grammar, &mut parser, source).unwrap();
    (grammar, tree)
}

fn json_query_tree(source: &str) -> (Language, Forest) {
    query_tree(json_language(), source)
}

fn provider_results<Provider, Chunk>(
    query: &Query,
    tree: &Forest,
    provider: Provider,
    mode: usize,
    optimized: bool,
) -> Vec<QuerySnapshot>
where
    Provider: tree_squatter::TextProvider<Chunk>,
    Chunk: AsRef<[u8]>,
{
    use tree_squatter::StreamingIterator;
    let mut cursor = QueryCursor::new();
    cursor.set_optimized(optimized);
    let mut results = Vec::new();
    match mode {
        0 | 1 => {
            let mut execution = if mode == 0 {
                cursor.execute(query, tree.root_node(), provider)
            } else {
                cursor.execute_with_options(
                    query,
                    tree.root_node(),
                    provider,
                    QueryCursorOptions::new(),
                )
            };
            while let Some(found) = execution.next_match() {
                results.push(snapshot(&found, None));
            }
            assert_eq!(execution.error(), None);
        }
        2 | 3 => {
            let mut matches = if mode == 2 {
                cursor.matches(query, tree.root_node(), provider)
            } else {
                cursor.matches_with_options(
                    query,
                    tree.root_node(),
                    provider,
                    QueryCursorOptions::new(),
                )
            };
            while let Some(found) = matches.next() {
                results.push(snapshot(found, None));
            }
        }
        4 | 5 => {
            let mut captures = if mode == 4 {
                cursor.captures(query, tree.root_node(), provider)
            } else {
                cursor.captures_with_options(
                    query,
                    tree.root_node(),
                    provider,
                    QueryCursorOptions::new(),
                )
            };
            while let Some((found, index)) = captures.next() {
                results.push(snapshot(found, Some(*index)));
            }
        }
        6 => {
            let mut execution = cursor.execute(query, tree.root_node(), provider);
            while let Some((found, index)) = execution.next_capture() {
                results.push(snapshot(&found, Some(index)));
            }
        }
        _ => unreachable!(),
    }
    results
}

#[test]
fn chunked_predicates_and_streaming_entry_points() {
    let source = "[\"héllo\",\"héllo\",\"world\",\"\",12,12,34]";
    let (grammar, tree) = json_query_tree(source);
    for pattern in [
        "((string_content) @text (#eq? @text \"héllo\"))",
        "((string_content) @text (#not-eq? @text \"héllo\"))",
        "((string_content) @text (#match? @text \"^hé.*lo$\"))",
        "((string_content) @text (#not-match? @text \"é.*lo\"))",
        "((string_content) @text (#any-of? @text \"héllo\" \"world\"))",
        "((string_content) @text (#not-any-of? @text \"world\"))",
        "((array (number)+ @number) (#eq? @number \"12\"))",
        "((array (number)+ @number) (#any-eq? @number \"12\"))",
        "((array (number)+ @number) (#any-not-eq? @number \"12\"))",
        "((array (number)+ @number) (#any-match? @number \"^12$\"))",
        "((array (number)+ @number) (#any-not-match? @number \"^12$\"))",
        "((array (number)+ @number) (#not-any-of? @number \"34\"))",
        "((array (number) @left (number) @right) (#eq? @left @right))",
        "((array (number)+ @left (number)+ @right) (#any-not-eq? @left @right))",
    ] {
        let query = Query::new(&grammar, pattern).unwrap();
        for optimized in [false, true] {
            for mode in 0..7 {
                let expected = provider_results(&query, &tree, source.as_bytes(), mode, optimized);
                // Every byte boundary includes splits inside UTF-8 and regex matches.
                for width in [1, 2, 5, source.len()] {
                    let borrowed = |node: tree_squatter::Node<'_>| {
                        source.as_bytes()[node.byte_range()].chunks(width)
                    };
                    assert_eq!(
                        provider_results(&query, &tree, borrowed, mode, optimized),
                        expected,
                        "{pattern}, mode={mode}, width={width}"
                    );
                    let owned = |node: tree_squatter::Node<'_>| {
                        let mut chunks = vec![Vec::new()];
                        for chunk in source.as_bytes()[node.byte_range()].chunks(width) {
                            chunks.push(chunk.to_vec());
                            chunks.push(Vec::new());
                        }
                        chunks.into_iter()
                    };
                    assert_eq!(
                        provider_results(&query, &tree, owned, mode, optimized),
                        expected
                    );
                }
                assert_eq!(
                    expected,
                    provider_results(
                        &query,
                        &tree,
                        source.as_bytes(),
                        if mode < 4 { 0 } else { 6 },
                        optimized
                    )
                );
            }
        }
    }
    for predicate in [
        "eq? @text \"\"",
        "match? @text \"^$\"",
        "any-of? @text \"\"",
        "not-eq? @text \"x\"",
    ] {
        let query = Query::new(&grammar, &format!("((string) @text (#{predicate}))")).unwrap();
        for mode in 0..7 {
            let empty = |_: tree_squatter::Node<'_>| std::iter::empty::<Vec<u8>>();
            let empty_chunk = |_: tree_squatter::Node<'_>| std::iter::once(Vec::<u8>::new());
            let expected = provider_results(&query, &tree, empty_chunk, mode, true);
            assert!(!expected.is_empty());
            assert_eq!(provider_results(&query, &tree, empty, mode, true), expected);
        }
    }
}

fn assert_query_metadata(query: &Query, reference: &tree_sitter::Query, source: &str) {
    assert_eq!(query.pattern_count(), reference.pattern_count(), "{source}");
    assert_eq!(query.capture_names(), reference.capture_names(), "{source}");
    for pattern in 0..reference.pattern_count() {
        let index = tree_squatter::PatternIx(pattern);
        assert_eq!(
            query.capture_quantifiers(index),
            reference.capture_quantifiers(pattern)
        );
        assert_eq!(
            query.start_byte_for_pattern(index),
            reference.start_byte_for_pattern(pattern)
        );
        assert_eq!(
            query.end_byte_for_pattern(index),
            reference.end_byte_for_pattern(pattern)
        );
        assert_eq!(
            query.is_pattern_rooted(index),
            reference.is_pattern_rooted(pattern)
        );
        assert_eq!(
            query.is_pattern_non_local(index),
            reference.is_pattern_non_local(pattern)
        );
    }
    for offset in 0..source.len() {
        assert_eq!(
            query.is_pattern_guaranteed_at_step(offset),
            reference.is_pattern_guaranteed_at_step(offset),
            "offset {offset}"
        );
    }
}

fn assert_query_error(actual: &tree_squatter::QueryError, expected: &tree_sitter::QueryError) {
    assert_eq!(actual.to_string(), expected.to_string());
    assert_eq!(
        (
            actual.row,
            actual.column,
            actual.offset,
            &actual.kind,
            &actual.message
        ),
        (
            expected.row,
            expected.column,
            expected.offset,
            &expected.kind,
            &expected.message
        ),
    );
}

#[test]
fn metadata_diagnostics_and_independent_clones() {
    use tree_squatter::{CaptureIx, PatternIx, QueryPredicateArg, QueryProperty};
    let source = "[1,2,3]";
    let (grammar, tree) = json_query_tree(source);
    for source in [
        "(_) @node",
        "(pair key: (string) @key value: (_) @value)",
        "(array [(number) (string)]+ @item)",
        "((number) @first (number) @second)",
        "((number)+ @numbers)",
        "((string) @text (#match? @text \"a\"))",
        "(array . (number)? @first . (number)* @rest .)",
        "(not_a_node) @capture",
        "(pair invalid_field: (_))",
        "(",
    ] {
        match (
            Query::new(&grammar, source),
            tree_sitter::Query::new(&grammar.tree_sitter_language(), source),
        ) {
            (Ok(query), Ok(reference)) => assert_query_metadata(&query, &reference, source),
            (Err(actual), Err(expected)) => assert_query_error(&actual, &expected),
            _ => panic!("different compilation result for {source}"),
        }
    }
    let patterns = "((array (number)+ @number) @array (#set! @number key \"value\") (#is? local) (#is-not? @array marked \"yes\") (#custom! @number \"text\"))\n(number) @single";
    let mut query = Query::new(&grammar, patterns).unwrap();
    let reference = tree_sitter::Query::new(&grammar.tree_sitter_language(), patterns).unwrap();
    assert_eq!(query.capture_index_for_name("number"), Some(CaptureIx(0)));
    assert_eq!(query.capture_index_for_name("absent"), None);
    assert_query_metadata(&query, &reference, patterns);
    assert_eq!(
        query.property_settings(PatternIx(0)),
        &[QueryProperty::new("key", Some("value"), Some(CaptureIx(0)))]
    );
    assert_eq!(
        query.property_predicates(PatternIx(0)),
        &[
            (QueryProperty::new("local", None, None), true),
            (
                QueryProperty::new("marked", Some("yes"), Some(CaptureIx(1))),
                false
            ),
        ]
    );
    let general = query.general_predicates(PatternIx(0));
    assert_eq!(general.len(), 1);
    assert_eq!(&*general[0].operator, "custom!");
    assert_eq!(
        &*general[0].args,
        &[
            QueryPredicateArg::Capture(CaptureIx(0)),
            QueryPredicateArg::String("text".into())
        ]
    );
    query.disable_pattern(PatternIx(1));
    query.disable_capture("array");
    let mut cloned = query.deep_clone();
    let expected = provider_results(&query, &tree, source.as_bytes(), 0, true);
    assert_eq!(
        provider_results(&cloned, &tree, source.as_bytes(), 0, true),
        expected
    );
    query.disable_capture("number");
    drop(query);
    assert_eq!(cloned.capture_names(), reference.capture_names());
    assert_eq!(
        provider_results(&cloned, &tree, source.as_bytes(), 0, true),
        expected
    );
    cloned.disable_pattern(PatternIx(0));
    assert!(provider_results(&cloned, &tree, source.as_bytes(), 0, true).is_empty());

    for pattern in [
        "\n(not_a_node)",
        "\n(pair nonexistent: (_))",
        "((number) (#eq? @missing \"a\"))",
        "\n(",
        "(number (number))",
        "((number) @number (#@number))",
    ] {
        let actual = Query::new(&grammar, pattern).err().unwrap();
        let expected = tree_sitter::Query::new(&grammar.tree_sitter_language(), pattern)
            .err()
            .unwrap();
        assert_query_error(&actual, &expected);
    }
}

#[test]
fn cloned_queries_preserve_compact_error_symbols() {
    let source = "[1, {broken}, 2]";
    let (grammar, tree) = json_query_tree(source);
    assert!(tree.root_node().has_error());
    for pattern in ["(_) @node", "(ERROR) @error", "(array (_) @child) @array"] {
        let query = Query::new(&grammar, pattern).unwrap();
        let cloned = query.deep_clone();
        for optimized in [false, true] {
            for mode in [0, 6] {
                assert_eq!(
                    provider_results(&cloned, &tree, source.as_bytes(), mode, optimized),
                    provider_results(&query, &tree, source.as_bytes(), mode, optimized),
                    "{pattern}, mode={mode}, optimized={optimized}"
                );
            }
        }
    }
}

#[test]
fn predicate_diagnostics_match_tree_sitter() {
    let (grammar, _) = json_query_tree("0");
    let mut predicates = Vec::new();
    for (operators, arguments) in [
        (
            &["eq?", "not-eq?", "any-eq?", "any-not-eq?"][..],
            &[
                "",
                "@number",
                "@number one two",
                "\"café\" @other",
                "literal text",
            ][..],
        ),
        (
            &["match?", "not-match?", "any-match?", "any-not-match?"][..],
            &[
                "",
                "@number",
                "@number one two",
                "literal text",
                "@number @other",
                "@number \"[\"",
                "literal \"[\"",
                "literal @other",
            ][..],
        ),
        (
            &["any-of?", "not-any-of?"][..],
            &[
                "",
                "literal",
                "literal @other",
                "@number @other",
                "@number one @other",
            ][..],
        ),
        (
            &["set!", "is?", "is-not?"][..],
            &[
                "",
                "@number",
                "@number @other",
                "@number @other key",
                "key @number @other",
                "key value extra",
                "key value extra fourth",
            ][..],
        ),
    ] {
        for operator in operators {
            for arguments in arguments {
                predicates.push(format!("#{operator} {arguments}"));
            }
        }
    }
    for prefix in [
        "",
        "\n  ",
        "; café\n(string) @previous\n  ",
        "(string) @previous ",
    ] {
        for predicate in &predicates {
            let source = format!("{prefix}((number) @number @other\n  ({predicate}))");
            let actual = Query::new(&grammar, &source).err().unwrap();
            let expected = tree_sitter::Query::new(&grammar.tree_sitter_language(), &source)
                .err()
                .unwrap();
            assert_eq!(
                expected.kind,
                tree_squatter::QueryErrorKind::Predicate,
                "{source}"
            );
            assert_query_error(&actual, &expected);
        }
    }
}

#[test]
fn removal_keeps_current_captures_readable_and_nodes_independent() {
    use tree_squatter::StreamingIterator;
    let source = "[1,2,3,4]";
    let (grammar, tree) = json_query_tree(source);
    let query = Query::new(&grammar, "(array (number)+ @number) @array").unwrap();
    for optimized in [false, true] {
        let mut cursor = QueryCursor::new();
        cursor.set_optimized(optimized);
        for explicit in [false, true] {
            let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
            let (found, _) = execution.next_capture().unwrap();
            let id = found.id();
            let captures = found.captures();
            let saved = captures.to_vec();
            if !explicit {
                found.remove();
                found.remove();
                assert_eq!(captures, saved);
            }
            let node = found
                .nodes_for_capture_index(query.capture_index_for_name("number").unwrap())
                .next()
                .unwrap();
            if explicit {
                execution.remove_match(id);
                execution.remove_match(id);
            }
            while let Some((found, _)) = execution.next_capture() {
                assert_ne!(found.id(), id);
            }
            drop(execution);
            assert_eq!(node.utf8_text(source.as_bytes()).unwrap(), "1");
        }
        let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
        let (found, _) = captures.next().unwrap();
        let id = found.id();
        let borrowed = found.captures();
        let saved = borrowed.to_vec();
        found.remove();
        found.remove();
        assert_eq!(borrowed, saved);
        // Moving an iterator after its first result must not invalidate removal state.
        let mut moved = captures;
        moved.set_byte_range(0..0);
        moved.get().unwrap().0.remove();
        while let Some((found, _)) = moved.next() {
            assert_ne!(found.id(), id);
        }
        drop(moved);
        let query = Query::new(
            &grammar,
            "(array (number) @number (number) @number (number) @number (number) @number) @array",
        )
        .unwrap();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        let found = matches.next().unwrap();
        let nodes: Vec<_> = found
            .nodes_for_capture_index(query.capture_index_for_name("number").unwrap())
            .collect();
        assert_eq!(nodes.len(), 4);
        assert_eq!(found.captures().len(), 5);
        found.remove();
        assert!(matches.next().is_none());
        drop(matches);
        assert_eq!(nodes[0].utf8_text(source.as_bytes()).unwrap(), "1");
        assert_eq!(
            provider_results(&query, &tree, source.as_bytes(), 4, optimized).len(),
            5
        );
    }
}

#[test]
fn progress_cancellation_resumes_every_entry_point() {
    use std::cell::Cell;
    use tree_squatter::{QueryCursorState, StreamingIterator};
    let source = format!("[{}]", "[1,2,3,4],".repeat(600).trim_end_matches(','));
    let (grammar, mut tree) = json_query_tree(&source);
    for side_data in [true, false] {
        if !side_data {
            tree.drop_point_data();
            tree.drop_presence_cache();
        }
        for pattern in [
            "(number) @number",
            "(array . (number) @first . (number) @second . (number) @third . (number) @last .)",
            "(array (number)* @left (number)* @right) @array",
            "((number) @left . (number) @right)",
            "(string) @absent",
            "((number) @number (#eq? @number \"absent\"))",
        ] {
            let query = Query::new(&grammar, pattern).unwrap();
            for optimized in [false, true] {
                for mode in [0, 2, 4, 6] {
                    let expected =
                        provider_results(&query, &tree, source.as_bytes(), mode, optimized);
                    let calls = Cell::new(0);
                    let requested = Cell::new(false);
                    let mut positions = Vec::new();
                    let mut progress = |state: &QueryCursorState| {
                        positions.push(state.current_byte_offset());
                        calls.set(calls.get() + 1);
                        // Repeated stops also cover searches that never produce a result.
                        if calls.get() <= 4 {
                            requested.set(true);
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        }
                    };
                    let mut options = QueryCursorOptions::new().progress_callback(&mut progress);
                    let mut cursor = QueryCursor::new();
                    cursor.set_optimized(optimized);
                    let mut actual = Vec::new();
                    let mut stops = 0;
                    macro_rules! collect {
                        ($stream:ident, $next:ident, $append:expr) => {
                            loop {
                                if let Some(found) = $stream.$next() {
                                    actual.push(($append)(found));
                                } else if requested.replace(false) {
                                    stops += 1;
                                } else {
                                    break;
                                }
                            }
                        };
                    }
                    match mode {
                        0 | 6 => {
                            let mut execution = cursor.execute_with_options(
                                &query,
                                tree.root_node(),
                                source.as_bytes(),
                                options.reborrow(),
                            );
                            if mode == 0 {
                                collect!(execution, next_match, |found| snapshot(&found, None));
                            } else {
                                collect!(execution, next_capture, |(found, index)| snapshot(
                                    &found,
                                    Some(index)
                                ));
                            }
                            assert_eq!(execution.error(), None);
                        }
                        2 => {
                            let mut matches = cursor.matches_with_options(
                                &query,
                                tree.root_node(),
                                source.as_bytes(),
                                options.reborrow(),
                            );
                            collect!(matches, next, |found| snapshot(found, None));
                        }
                        4 => {
                            let mut captures = cursor.captures_with_options(
                                &query,
                                tree.root_node(),
                                source.as_bytes(),
                                options.reborrow(),
                            );
                            collect!(captures, next, |(found, index): &(
                                _,
                                tree_squatter::MatchCaptureIx
                            )| snapshot(
                                found,
                                Some(*index)
                            ));
                        }
                        _ => unreachable!(),
                    }
                    assert_eq!(
                        actual, expected,
                        "{pattern}, mode={mode}, optimized={optimized}, points={side_data}"
                    );
                    // A fresh execution starts from the root with reusable options.
                    calls.set(calls.get().max(4));
                    let mut fresh = cursor.matches_with_options(
                        &query,
                        tree.root_node(),
                        source.as_bytes(),
                        options.reborrow(),
                    );
                    let mut fresh_results = Vec::new();
                    while let Some(found) = fresh.next() {
                        fresh_results.push(snapshot(found, None));
                    }
                    assert_eq!(
                        fresh_results,
                        provider_results(&query, &tree, source.as_bytes(), 0, optimized)
                    );
                    drop(fresh);
                    drop(options);
                    assert!(calls.get() > 0);
                    assert!(stops > 0);
                    assert!(positions.iter().all(|&position| position <= source.len()));
                    // Optimized searches must report their local progress, including no-hit scans.
                    assert!(positions.iter().any(|&position| position > 0));
                }
            }
        }
    }
}

#[test]
fn optimized_capture_progress_tracks_later_subtrees() {
    use std::cell::Cell;
    use tree_squatter::QueryCursorState;

    let nested = (0..150).fold("[[],[]]".to_string(), |nested, _| format!("[{nested},[]]"));
    let source = format!("[{nested},{nested},{nested}]");
    let second_start = nested.len() + 2;
    let (grammar, tree) = json_query_tree(&source);
    let query = Query::new(&grammar, "(array . (array) @first . (array) @last .)").unwrap();
    let expected = provider_results(&query, &tree, source.as_bytes(), 6, true);

    for cancel in [false, true] {
        let reached_second = Cell::new(false);
        let requested = Cell::new(false);
        let mut positions = Vec::new();
        let mut progress = |state: &QueryCursorState| {
            if reached_second.get() {
                positions.push(state.current_byte_offset());
                if cancel && positions.len() <= 3 {
                    requested.set(true);
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        };
        let mut cursor = QueryCursor::new();
        let mut execution = cursor.execute_with_options(
            &query,
            tree.root_node(),
            source.as_bytes(),
            QueryCursorOptions::new().progress_callback(&mut progress),
        );
        let mut actual = Vec::new();
        let mut stops = 0;
        loop {
            if let Some((found, index)) = execution.next_capture() {
                let node = found.captures()[index.raw() as usize].node;
                if node.start_byte() >= second_start {
                    reached_second.set(true);
                }
                actual.push(snapshot(&found, Some(index)));
            } else if requested.replace(false) {
                stops += 1;
            } else {
                break;
            }
        }
        drop(execution);
        assert_eq!(actual, expected);
        assert_eq!(stops, if cancel { 3 } else { 0 });
        assert!(!positions.is_empty());
        // The direct traversal cannot revisit the root after entering a later subtree.
        assert!(
            positions
                .iter()
                .all(|&position| (second_start..source.len()).contains(&position))
        );
    }
}

#[test]
fn cursor_and_iterator_ranges_narrow_validate_and_persist() {
    use tree_sitter::Point;
    use tree_squatter::StreamingIterator;
    let source = "[1,2,3,4]";
    let (grammar, tree) = json_query_tree(source);
    let query = Query::new(&grammar, "(number) @number").unwrap();
    let reference_query =
        tree_sitter::Query::new(&grammar.tree_sitter_language(), "(number) @number").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&grammar.tree_sitter_language())
        .unwrap();
    let reference_tree = parser.parse(source, None).unwrap();
    let mut ranges = vec![0..0, 3..6, 4..4, 7..2, 4..0];
    if usize::BITS > 32 {
        let wide = (u32::MAX as usize) + 1;
        ranges.extend([wide + 3..wide + 6, wide + 4..wide, wide + 7..wide + 2]);
    }
    for optimized in [false, true] {
        for range in ranges.clone() {
            for (points, containing) in [(false, false), (true, false), (false, true), (true, true)]
            {
                let mut reference = tree_sitter::QueryCursor::new();
                let mut cursor = QueryCursor::new();
                cursor.set_optimized(optimized);
                // Reversed ranges must keep a previously stored restriction.
                if containing && points {
                    reference.set_containing_point_range(Point::new(0, 3)..Point::new(0, 6));
                    cursor.set_containing_point_range(Point::new(0, 3)..Point::new(0, 6));
                    reference.set_containing_point_range(
                        Point::new(0, range.start)..Point::new(0, range.end),
                    );
                    cursor.set_containing_point_range(
                        Point::new(0, range.start)..Point::new(0, range.end),
                    );
                } else if containing {
                    reference
                        .set_containing_byte_range(3..6)
                        .set_containing_byte_range(range.clone());
                    cursor
                        .set_containing_byte_range(3..6)
                        .set_containing_byte_range(range.clone());
                } else if points {
                    reference.set_point_range(Point::new(0, 3)..Point::new(0, 6));
                    cursor.set_point_range(Point::new(0, 3)..Point::new(0, 6));
                    reference.set_point_range(Point::new(0, range.start)..Point::new(0, range.end));
                    cursor.set_point_range(Point::new(0, range.start)..Point::new(0, range.end));
                } else {
                    reference.set_byte_range(3..6).set_byte_range(range.clone());
                    cursor.set_byte_range(3..6).set_byte_range(range.clone());
                }
                let mut expected = Vec::new();
                let mut matches = reference.matches(
                    &reference_query,
                    reference_tree.root_node(),
                    source.as_bytes(),
                );
                while let Some(found) = matches.next() {
                    expected.push(found.captures()[0].node.byte_range());
                }
                let mut actual = Vec::new();
                let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
                while let Some(found) = matches.next() {
                    actual.push(found.captures()[0].node.byte_range());
                }
                assert_eq!(
                    actual, expected,
                    "{range:?}, points={points}, containing={containing}, optimized={optimized}"
                );
            }
        }
        let mut cursor = QueryCursor::new();
        cursor.set_optimized(optimized);
        cursor.set_match_limit(17);
        assert_eq!(cursor.match_limit(), 17);
        cursor.set_max_start_depth(Some(0));
        assert!(
            cursor
                .matches(&query, tree.root_node(), source.as_bytes())
                .next()
                .is_none()
        );
        cursor.set_max_start_depth(None);
        if usize::BITS > 32 {
            let wide = (u32::MAX as usize) + 1;
            cursor.set_point_range(
                tree_sitter::Point::new(wide, wide + 5)..tree_sitter::Point::new(wide, wide + 6),
            );
            assert_eq!(
                cursor
                    .matches(&query, tree.root_node(), source.as_bytes())
                    .next()
                    .unwrap()
                    .captures()[0]
                    .node
                    .start_byte(),
                5
            );
            cursor.set_point_range(tree_sitter::Point::new(0, 0)..tree_sitter::Point::new(0, 0));
        }
        {
            let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
            assert_eq!(matches.next().unwrap().captures()[0].node.start_byte(), 1);
            matches.set_byte_range(5..8);
            matches.set_point_range(Point::new(0, 5)..Point::new(0, 8));
            assert_eq!(matches.next().unwrap().captures()[0].node.start_byte(), 5);
            assert_eq!(matches.next().unwrap().captures()[0].node.start_byte(), 7);
            assert!(matches.next().is_none());
        }
        {
            let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
            let (found, index) = captures.next().unwrap();
            assert_eq!(found.captures()[index.raw() as usize].node.start_byte(), 5);
            captures.set_byte_range(7..8);
            captures.set_point_range(Point::new(0, 7)..Point::new(0, 8));
            let (found, index) = captures.next().unwrap();
            assert_eq!(found.captures()[index.raw() as usize].node.start_byte(), 7);
            assert!(captures.next().is_none());
        }
        let found = cursor
            .execute(&query, tree.root_node(), source.as_bytes())
            .next_match()
            .unwrap()
            .captures()[0]
            .node;
        assert_eq!(found.start_byte(), 7);
        cursor
            .set_byte_range(0..0)
            .set_point_range(Point::new(0, 0)..Point::new(0, 0));
        assert_eq!(
            cursor
                .matches(&query, tree.root_node(), source.as_bytes())
                .count(),
            4
        );
    }
}

#[test]
fn query_language_mismatch_is_an_execution_error() {
    let (_, tree) = json_query_tree("[1]");
    let language = c_language();
    let grammar = Language::new(&language).unwrap();
    let query = Query::new(&grammar, "(_) @node").unwrap();
    for optimized in [false, true] {
        let mut cursor = QueryCursor::new();
        cursor.set_optimized(optimized);
        let mut execution = cursor.execute(&query, tree.root_node(), b"[1]".as_slice());
        assert_eq!(
            execution.error(),
            Some(tree_squatter::QueryExecutionError::InvalidExecution)
        );
        assert!(execution.next_match().is_none());
        assert!(execution.next_capture().is_none());
    }
}
