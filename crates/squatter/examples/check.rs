//! Run inside a grammar container: check LIBRARY SYMBOL.
use std::error::Error;
use tree_sitter::StreamingIterator;
use tree_sitter_squatter::{
    PackOptions, Tree,
    traits::{NodeLike, TreeLike},
};

fn check_queries(
    language: &tree_sitter::Language,
    source: &[u8],
    mainline: &tree_sitter::Tree,
    packed: &Tree,
) -> Result<(), Box<dyn Error>> {
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
            let mut actual_query = tree_sitter_squatter::Query::new(language, source_query)?;
            if modification == 2 {
                let name = actual_query.capture_names()[0].clone();
                expected_query.disable_capture(&name);
                actual_query.disable_capture(&name);
            }
            for captures in [false, true] {
                let mut expected_cursor = tree_sitter::QueryCursor::new();
                let mut expected = Vec::new();
                let mut append = |result: &tree_sitter::QueryMatch<'_, '_>, index| {
                    expected.push((
                        result.pattern_index,
                        index,
                        result
                            .captures()
                            .iter()
                            .map(|capture| {
                                (
                                    capture.index,
                                    capture.node.start_byte(),
                                    capture.node.end_byte(),
                                    capture.node.kind_id(),
                                )
                            })
                            .collect::<Vec<_>>(),
                    ));
                };
                if captures {
                    let mut results =
                        expected_cursor.captures(&expected_query, mainline.root_node(), source);
                    while let Some((result, index)) = results.next() {
                        append(result, Some(*index));
                    }
                } else {
                    let mut results =
                        expected_cursor.matches(&expected_query, mainline.root_node(), source);
                    while let Some(result) = results.next() {
                        append(result, None);
                    }
                }
                let mut actual_cursor = tree_sitter_squatter::QueryCursor::new();
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
                                (
                                    capture.index,
                                    capture.node.start_byte(),
                                    capture.node.end_byte(),
                                    capture.node.kind_id(),
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
    use tree_sitter_squatter::{Query, QueryCursor, QueryExecutionError};
    let mut cursor = QueryCursor::new();
    cursor.set_timeout(Some(std::time::Duration::from_secs(1)));
    for _ in 0..3 {
        let packed = Tree::pack(tree)?;
        let query = Query::new(language, "(_) @node")?;
        let mut execution = cursor.execute(&query, packed.root_node(), b"");
        assert!(execution.next_capture().is_some());
    }
    // This checkout's mainline disable_pattern leaves the wildcard-root count
    // stale and asserts. Verify the intended behavior directly for this case.
    let packed = Tree::pack(tree)?;
    let mut query = Query::new(language, "(_) @a (_) @b")?;
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
    let query = Query::new(language, "(_ (_)+ @child) @parent")?;
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

fn kinds<T: TreeLike>(tree: &T) -> Vec<u16> {
    fn visit<'tree, N: NodeLike<'tree>>(node: N, output: &mut Vec<u16>) {
        output.push(node.attributes().kind_id);
        let mut index = 0;
        while let Some(child) = node.child(index) {
            visit(child, output);
            index += 1;
        }
    }
    let mut output = Vec::new();
    visit(tree.root(), &mut output);
    output
}

fn check_cursor(tree: &Tree) -> Result<(), Box<dyn Error>> {
    let mut cursor = tree.root_node().walk()?;
    loop {
        assert_eq!(cursor.attributes(), cursor.node().attributes());
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(());
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.len() != 2 {
        return Err("usage: check LIBRARY SYMBOL".into());
    }
    // The library outlives every language, parser and packed tree in this scope.
    let library = unsafe { libloading::Library::new(&arguments[0])? };
    let get_language: libloading::Symbol<
        unsafe extern "C" fn() -> *const tree_sitter::ffi::TSLanguage,
    > = unsafe { library.get(arguments[1].as_bytes())? };
    let language = unsafe { tree_sitter::Language::from_raw(get_language()) };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language)?;
    let source = b"{\"a\": [1, true, null], \"b\": 2}";
    let mainline = parser.parse(source, None).ok_or("parse failed")?;
    let packed = Tree::pack_with_options(
        &mainline,
        PackOptions {
            initial_group_capacity: 1,
            ..Default::default()
        },
    )?;
    assert_eq!(kinds(&mainline), kinds(&packed));
    assert_eq!(
        packed.root_node().attributes(),
        mainline.root_node().attributes()
    );
    assert_eq!(
        packed.root_node().preorder().count(),
        packed.root_node().descendant_count()
    );
    for node in packed.root_node().preorder() {
        assert_eq!(node.children().count(), node.child_count());
        assert_eq!(node.named_children().count(), node.named_child_count());
        assert_eq!(node.preorder().count(), node.descendant_count());
    }
    check_cursor(&packed)?;
    check_queries(&language, source, &mainline, &packed)?;
    check_cursor_reuse(&language, &mainline)?;
    drop(mainline);
    let compact = packed.repack()?;
    let decoded = Tree::from_bytes(&language, compact.as_bytes())?;
    assert_eq!(kinds(&packed), kinds(&decoded));
    check_cursor(&decoded)?;
    assert_eq!(compact.group_count(), compact.group_capacity());
    let mut corrupted = compact.as_bytes().to_vec();
    corrupted[0] ^= 0x80;
    assert!(Tree::from_bytes(&language, &corrupted).is_err());
    println!(
        "ok: Rust FFI, ownership, traits, iterators, persistence and streaming queries ({} nodes)",
        packed.root_node().descendant_count()
    );
    Ok(())
}
