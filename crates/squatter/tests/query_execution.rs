use std::time::Duration;
use tree_squatter::{Language, Query, QueryCursor, Tree};

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
    let grammar = Language::new(&language).unwrap();
    let reference_grammar = tree_squatter::Language::new(&language).unwrap();
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
fn error_queries_survive_native_mutations() {
    use tree_sitter::StreamingIterator;

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
        let tree = Tree::pack(&grammar, &native).unwrap();
        assert!(tree.root_node().has_error());

        for pattern in [
            "(ERROR) @error (_) @node",
            "(ERROR) @error (_ (_) @child) @parent",
        ] {
            let mut query = Query::new(&grammar, pattern).unwrap();
            let reference = tree_sitter::Query::new(&language, pattern).unwrap();
            let error_capture = reference.capture_index_for_name("error").unwrap();
            let mut reference_cursor = tree_sitter::QueryCursor::new();
            let mut matches =
                reference_cursor.matches(&reference, native.root_node(), source.as_bytes());
            let mut expected = Vec::new();
            while let Some(result) = matches.next() {
                expected.push((
                    result.pattern_index,
                    result
                        .captures()
                        .iter()
                        .map(|capture| {
                            (
                                capture.node.byte_range(),
                                capture.node.kind_id(),
                                capture.index,
                            )
                        })
                        .collect::<Vec<_>>(),
                ));
            }
            assert!(expected.iter().any(|(pattern, _)| *pattern == 0));

            for mutation in 0..4 {
                match mutation {
                    1 => {
                        query.disable_capture("error");
                        for (_, captures) in &mut expected {
                            captures.retain(|(_, _, index)| *index != error_capture);
                        }
                    }
                    2 | 3 => {
                        let pattern = 3 - mutation;
                        query.disable_pattern(pattern);
                        expected.retain(|(index, _)| *index != pattern);
                    }
                    _ => {}
                }

                for optimized in [false, true] {
                    let mut cursor = QueryCursor::new();
                    cursor.set_optimized(optimized);
                    let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
                    let mut actual = Vec::new();
                    while let Some(result) = execution.next_match() {
                        actual.push((
                            result.pattern_index,
                            result
                                .captures
                                .iter()
                                .map(|capture| {
                                    (
                                        capture.node.byte_range(),
                                        capture.node.kind_id().get(),
                                        capture.index,
                                    )
                                })
                                .collect::<Vec<_>>(),
                        ));
                    }
                    assert_eq!(execution.error(), None);
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
        let tree = Tree::pack(&grammar, &native).unwrap();
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
                    let field = language.field_name_for_id(field.get()).unwrap();
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
            let tree = Tree::pack(&grammar, &native).unwrap();
            let capturable_slots = tree
                .root_node()
                .preorder()
                .nodes()
                .filter(|node| node.end_byte() > 0)
                .map(|node| node.slot().get())
                .collect::<BTreeSet<_>>();
            for (pattern, query) in &queries {
                for bounded in [false, true] {
                    let mut optimized = QueryCursor::new();
                    let mut reference = QueryCursor::new();
                    reference.set_optimized(false);
                    if bounded {
                        let start = source.len() / 2;
                        assert!(optimized.set_byte_range(start..start + 1));
                        assert!(reference.set_byte_range(start..start + 1));
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
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
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
            let tree = Tree::parse(&grammar, &mut parser, &source).unwrap();
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
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
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
        assert!(cursor.set_byte_range(4..9));

        assert_eq!(
            matches!(&mut cursor, &query, tree, source),
            vec![
                (
                    0,
                    vec![(identifier.slot().get(), 0), (number.slot().get(), 1)]
                ),
                (1, vec![(identifier.slot().get(), 2)]),
            ],
        );

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
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let source = "int f() { return call(a, b, c, d, e, f, g, h); }\n".repeat(1000);
    let tree = Tree::parse(&grammar, &mut parser, &source).unwrap();
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

    cursor.set_timeout(Some(Duration::ZERO));
    let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
    let mut completed_matches = 0;
    while execution.next_match().is_some() {
        completed_matches += 1;
    }
    assert!(completed_matches < results.len());
    assert_eq!(execution.error(), None);
    drop(execution);

    cursor.set_timeout(None);
    cursor.set_match_limit(u32::MAX);
    assert!(cursor.set_byte_range(1..10));
    let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
    assert!(execution.next_match().is_none());
    assert_eq!(execution.error(), None);
    drop(execution);

    let query = Query::new(&grammar, "(identifier) @name").unwrap();
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
    let grammar = Language::new(&language).unwrap();
    let tree = Tree::pack(&grammar, &native).unwrap();
    let reference =
        tree_squatter::Tree::pack(&tree_squatter::Language::new(&language).unwrap(), &native)
            .unwrap();
    let pattern = "(identifier) @first (identifier) @second (identifier) @third";
    let query = Query::new(&grammar, pattern).unwrap();
    let reference_query = tree_squatter::Query::new(&grammar, pattern).unwrap();

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
            let tree = Tree::pack(&grammar, &native).unwrap();
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
                    for mode in 0..8 {
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
                            4 => {
                                reference.set_byte_range(4..4);
                                assert!(cursor.set_byte_range(4..4));
                            }
                            5 => {
                                reference.set_point_range(Point::new(1, 2)..Point::new(1, 3));
                                assert!(cursor.set_point_range(Point::new(1, 2)..Point::new(1, 3)));
                            }
                            6 => {
                                reference.set_byte_range(1..12);
                                assert!(cursor.set_byte_range(1..12));
                                reference.set_point_range(Point::new(1, 0)..Point::new(2, 0));
                                assert!(cursor.set_point_range(Point::new(1, 0)..Point::new(2, 0)));
                            }
                            7 => {
                                reference.set_byte_range(100..101);
                                assert!(cursor.set_byte_range(100..101));
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
                        assert_eq!(
                            execution.error(),
                            None,
                            "{pattern}, {source:?}, mode={mode}, optimized={optimized}"
                        );
                        expected_matches.sort();
                        actual_matches.sort();
                        assert_eq!(
                            expected_matches, actual_matches,
                            "{pattern}, {source:?}, mode={mode}, optimized={optimized}"
                        );
                        drop(execution);
                        let mut expected_captures = BTreeSet::new();
                        if mode >= 2 {
                            let mut execution = reference.captures(
                                &expected,
                                native.root_node(),
                                source.as_bytes(),
                            );
                            while let Some((result, index)) = execution.next() {
                                let capture = result.captures()[*index];
                                expected_captures.insert((
                                    result.pattern_index,
                                    (
                                        capture.node.start_byte(),
                                        capture.node.end_byte(),
                                        capture.node.kind_id(),
                                        capture.index,
                                    ),
                                ));
                            }
                        }
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
                        if mode >= 2 {
                            assert_eq!(
                                expected_captures, captured,
                                "{pattern}, {source:?}, mode={mode}, optimized={optimized}"
                            );
                        } else {
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
fn quantified_roots_with_ranges_match_tree_sitter() {
    use tree_sitter::{Point, StreamingIterator};

    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let source = "[1,2,3]";
    let native = parser.parse(source, None).unwrap();
    let tree = Tree::pack(&grammar, &native).unwrap();

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
                        assert!(cursor.set_point_range(range));
                    } else {
                        reference.set_byte_range(range.clone());
                        assert!(cursor.set_byte_range(range.clone()));
                    }

                    let mut execution =
                        reference.matches(&expected, native.root_node(), source.as_bytes());
                    let mut expected_matches = Vec::new();
                    while let Some(result) = execution.next() {
                        // Hidden repetition nodes affect the number of empty matches.
                        if !result.captures().is_empty() {
                            expected_matches.push(
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
                            );
                        }
                    }
                    let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
                    let mut actual_matches = Vec::new();
                    while let Some(result) = execution.next_match() {
                        if !result.captures.is_empty() {
                            actual_matches.push(
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
                            );
                        }
                    }
                    assert_eq!(execution.error(), None);
                    expected_matches.sort();
                    actual_matches.sort();
                    assert_eq!(
                        expected_matches, actual_matches,
                        "{pattern}, {range:?}, points={points}, optimized={optimized}"
                    );
                    drop(execution);

                    let mut execution =
                        reference.captures(&expected, native.root_node(), source.as_bytes());
                    let mut expected_captures = std::collections::BTreeSet::new();
                    while let Some((result, index)) = execution.next() {
                        let capture = result.captures()[*index];
                        expected_captures.insert((
                            capture.node.start_byte(),
                            capture.node.end_byte(),
                            capture.node.kind_id(),
                            capture.index,
                        ));
                    }
                    let mut execution = cursor.execute(&query, tree.root_node(), source.as_bytes());
                    let mut actual_captures = std::collections::BTreeSet::new();
                    while let Some((result, index)) = execution.next_capture() {
                        let capture = result.captures[index];
                        actual_captures.insert((
                            capture.node.start_byte(),
                            capture.node.end_byte(),
                            capture.node.kind_id().get(),
                            capture.index,
                        ));
                    }
                    assert_eq!(execution.error(), None);
                    assert_eq!(
                        expected_captures, actual_captures,
                        "{pattern}, {range:?}, points={points}, optimized={optimized}"
                    );
                }
            }
        }
    }
}

#[test]
fn disabled_rootless_and_branching_patterns_with_ranges() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let tree = Tree::parse(&grammar, &mut parser, "[1,2,3]").unwrap();
    for pattern in [
        "(_) @first\n(_) @node",
        "((_) @first (_) @second)\n(_) @node",
        "[(_) (_)] @first\n(_) @node",
    ] {
        let mut query = Query::new(&grammar, pattern).unwrap();
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
                        let mut found = false;
                        loop {
                            let result = if captures {
                                execution.next_capture().map(|(result, _)| result)
                            } else {
                                execution.next_match()
                            };
                            let Some(result) = result else { break };
                            assert!(!disabled || result.pattern_index != 0);
                            assert!(result.captures.iter().all(|capture| capture.index != 0));
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
