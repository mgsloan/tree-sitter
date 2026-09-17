//! Run inside a grammar container: check LIBRARY SYMBOL.
use std::error::Error;
use tree_sitter::StreamingIterator;
use tree_sitter_squatter::{
    KindSet, PackOptions, Tree,
    traits::{CursorLike, NodeIteratorLike, NodeLike, TreeLike},
};

fn check_shared_navigation<'tree, N: NodeLike<'tree>>(
    root: N,
    fields: u16,
) -> Result<(), Box<dyn Error>> {
    let mut cursor = root.cursor()?;
    let mut expected = Vec::new();
    loop {
        expected.push(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break;
            }
        }
        if cursor.depth() == 0 {
            break;
        }
    }
    assert!(root.preorder().collect::<Vec<_>>() == expected);
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
        let kinds = KindSet::new([node.kind_id(), root.kind_id(), node.kind_id(), u16::MAX]);
        let filtered: Vec<_> = expected
            .iter()
            .copied()
            .filter(|node| kinds.contains(node.kind_id()))
            .collect();
        assert!(root.descendants_matching_kinds(&kinds).collect::<Vec<_>>() == filtered);
    }
    let mut iterator = root.node_iterator()?;
    assert!(iterator.node().is_none());
    assert!(iterator.kind_id().is_none());
    assert!(iterator.byte_range().is_none());
    assert!(iterator.attributes().is_none());
    for &node in &expected {
        assert!(iterator.next() == Some(node));
        assert!(iterator.node() == Some(node));
        assert_eq!(iterator.kind_id(), Some(node.kind_id()));
        assert_eq!(iterator.byte_range(), Some(node.byte_range()));
        assert_eq!(iterator.attributes(), Some(node.attributes()));
        assert_eq!(iterator.kind_id(), Some(node.kind_id()));
    }
    for _ in 0..2 {
        assert!(iterator.next().is_none());
        assert!(iterator.node().is_none());
        assert!(iterator.kind_id().is_none());
        assert!(iterator.byte_range().is_none());
        assert!(iterator.attributes().is_none());
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
        assert_eq!(node.has_children(), node.child_count() != 0);
        assert_eq!(node.has_named_children(), node.named_child_count() != 0);
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
        for field in 0..=fields {
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

fn check_iterators(tree: &Tree) -> Result<(), Box<dyn Error>> {
    for root in tree.root_node().preorder().take(32) {
        let expected: Vec<_> = root.preorder().collect();
        let mut iterator = root.node_iterator()?;
        assert!(iterator.attributes().is_none());
        for &node in &expected {
            assert_eq!(iterator.next(), Some(node));
            assert_eq!(iterator.node(), Some(node));
            assert_eq!(iterator.attributes().unwrap(), node.attributes());
            assert_eq!(
                iterator.field_id(),
                (node.field_id() != 0).then_some(node.field_id())
            );
        }
        assert_eq!(iterator.next(), None);
        assert_eq!(iterator.next(), None);
        assert!(iterator.attributes().is_none());
        assert!(iterator.field_id().is_none());
        drop(iterator);
        assert_eq!(expected[0], root);
    }
    Ok(())
}

fn check_queries(
    language: &tree_sitter::Language,
    source: &[u8],
    mainline: &tree_sitter::Tree,
    packed: &Tree,
) -> Result<(), Box<dyn Error>> {
    check_iterators(packed)?;
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
    let grammar = tree_sitter_squatter::Grammar::new(language)?;
    let mut cursor = QueryCursor::new();
    cursor.set_timeout(Some(std::time::Duration::from_secs(1)));
    for _ in 0..3 {
        let packed = Tree::pack(&grammar, tree)?;
        let query = Query::new(language, "(_) @node")?;
        let mut execution = cursor.execute(&query, packed.root_node(), b"");
        assert!(execution.next_capture().is_some());
    }
    // This checkout's mainline disable_pattern leaves the wildcard-root count
    // stale and asserts. Verify the intended behavior directly for this case.
    let packed = Tree::pack(&grammar, tree)?;
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
    let grammar = tree_sitter_squatter::Grammar::new(&language)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language)?;
    let source = b"{\"a\": [1, true, null], \"b\": 2}";
    let mainline = parser.parse(source, None).ok_or("parse failed")?;
    let packed = Tree::pack_with_options(
        &grammar,
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
    check_shared_navigation(mainline.root_node(), language.field_count() as u16)?;
    check_shared_navigation(packed.root_node(), language.field_count() as u16)?;
    // Cross the presence-index threshold and several physical groups, retaining
    // a rare boolean beside common number and punctuation symbols.
    let large_source = format!("[true,{}null]", "123,\n".repeat(600));
    let large_native = parser.parse(&large_source, None).ok_or("parse failed")?;
    check_shared_navigation(large_native.root_node(), language.field_count() as u16)?;
    for points in [false, true] {
        for symbol_presence in [false, true] {
            let large_packed = Tree::pack_with_options(
                &grammar,
                &large_native,
                PackOptions {
                    points,
                    symbol_presence,
                    ..Default::default()
                },
            )?;
            check_shared_navigation(large_packed.root_node(), language.field_count() as u16)?;
        }
    }
    check_cursor(&packed)?;
    check_queries(&language, source, &mainline, &packed)?;
    check_cursor_reuse(&language, &mainline)?;
    let compact = packed.repack()?;
    let decoded = Tree::from_bytes(&grammar, compact.as_bytes())?;
    let borrowed = Tree::from_bytes_borrowed(&grammar, compact.as_bytes())?;
    assert_eq!(borrowed.as_bytes().as_ptr(), compact.as_bytes().as_ptr());
    assert_eq!(
        borrowed.root_node().preorder().count(),
        compact.root_node().preorder().count()
    );
    check_iterators(&borrowed)?;
    check_queries(&language, source, &mainline, &borrowed)?;
    drop(mainline);
    drop(borrowed);
    assert_eq!(kinds(&packed), kinds(&decoded));
    check_cursor(&decoded)?;
    assert_eq!(compact.group_count(), compact.group_capacity());
    let mut corrupted = compact.as_bytes().to_vec();
    corrupted[0] ^= 0x80;
    assert!(Tree::from_bytes(&grammar, &corrupted).is_err());
    println!(
        "ok: Rust FFI, ownership, traits, iterators, persistence and streaming queries ({} nodes)",
        packed.root_node().descendant_count()
    );
    Ok(())
}
