#![allow(dead_code)]

use std::{fmt::Debug, ops::Range};
use tree_sitter::{Language, Node, Query, QueryCursor, StreamingIterator, Tree};

pub fn json_language() -> Language {
    unsafe { Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) }
}

pub fn c_language() -> Language {
    unsafe { Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) }
}

pub fn c_sharp_language() -> Language {
    unsafe { Language::from_raw(tree_sitter_c_sharp::LANGUAGE.into_raw()().cast()) }
}

pub fn parse_native(language: &Language, source: impl AsRef<[u8]>) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(language).unwrap();
    parser.parse(source, None).unwrap()
}

pub type NodeDescription = (u16, usize, usize);
pub type CaptureDescription = (u32, NodeDescription);
pub type QueryResult = (usize, Option<usize>, Vec<CaptureDescription>);

pub fn describe_node(kind: impl Into<u16>, range: Range<usize>) -> NodeDescription {
    (kind.into(), range.start, range.end)
}

pub fn describe_capture(
    index: u32,
    kind: impl Into<u16>,
    range: Range<usize>,
) -> CaptureDescription {
    (index, describe_node(kind, range))
}

pub fn native_orders(root: Node<'_>) -> (Vec<NodeDescription>, Vec<NodeDescription>) {
    fn visit(
        node: Node<'_>,
        preorder: &mut Vec<NodeDescription>,
        postorder: &mut Vec<NodeDescription>,
    ) {
        let description = describe_node(node.kind_id(), node.byte_range());
        preorder.push(description);
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(child, preorder, postorder);
        }
        postorder.push(description);
    }
    let (mut preorder, mut postorder) = (Vec::new(), Vec::new());
    visit(root, &mut preorder, &mut postorder);
    (preorder, postorder)
}

pub fn native_query_results(
    query: &Query,
    root: Node<'_>,
    source: &[u8],
    captures: bool,
) -> Vec<QueryResult> {
    native_query_results_with_cursor(&mut QueryCursor::new(), query, root, source, captures)
}

pub fn native_query_results_with_cursor(
    cursor: &mut QueryCursor,
    query: &Query,
    root: Node<'_>,
    source: &[u8],
    captures: bool,
) -> Vec<QueryResult> {
    let mut results = Vec::new();
    let mut append = |result: &tree_sitter::QueryMatch<'_, '_>, index| {
        results.push((
            result.pattern_index,
            index,
            result
                .captures()
                .iter()
                .map(|capture| {
                    describe_capture(
                        capture.index,
                        capture.node.kind_id(),
                        capture.node.byte_range(),
                    )
                })
                .collect(),
        ));
    };
    if captures {
        let mut execution = cursor.captures(query, root, source);
        while let Some((result, index)) = execution.next() {
            append(result, Some(*index));
        }
    } else {
        let mut execution = cursor.matches(query, root, source);
        while let Some(result) = execution.next() {
            append(result, None);
        }
    }
    results
}

pub fn check_consumption<I: Iterator>(make: impl Fn() -> I, expected: &[I::Item])
where
    I::Item: Copy + Debug + PartialEq,
{
    for consumed in [0, 1, 2, 7, 16, 33, expected.len() + 1] {
        let mut items = make();
        for index in 0..consumed {
            assert_eq!(items.next(), expected.get(index).copied());
        }
        let remaining = &expected[consumed.min(expected.len())..];
        assert_eq!(items.count(), remaining.len());

        let mut items = make();
        for _ in 0..consumed {
            items.next();
        }
        let actual = items.fold(Vec::new(), |mut result, item| {
            result.push(item);
            result
        });
        assert_eq!(actual, remaining);
    }
    let mut items = make();
    for &item in expected {
        assert_eq!(items.next(), Some(item));
    }
    assert_eq!(items.next(), None);
    assert_eq!(items.next(), None);
}

pub fn assert_same_tree(actual: &tree_squatter::Forest, expected: &tree_squatter::Forest) {
    assert_eq!(actual.as_bytes(), expected.as_bytes());
    assert_eq!(
        actual.point_data().map(|points| points.as_bytes()),
        expected.point_data().map(|points| points.as_bytes()),
    );
    assert_eq!(
        actual.presence_cache().map(|cache| cache.as_bytes()),
        expected.presence_cache().map(|cache| cache.as_bytes()),
    );
}

pub fn pack_native(
    language: &Language,
    source: &str,
    options: tree_squatter::PackOptions,
) -> (Tree, tree_squatter::Forest) {
    let native = parse_native(language, source);
    let packed = tree_squatter::Forest::pack_with_options(
        &tree_squatter::Language::new(language).unwrap(),
        &native,
        options,
    )
    .unwrap();
    (native, packed)
}

pub fn query_snapshot(
    found: &tree_squatter::QueryMatch<'_, '_>,
    index: Option<tree_squatter::MatchCaptureIx>,
) -> QueryResult {
    (
        found.pattern_index.0,
        index.map(|index| index.raw() as usize),
        found
            .captures()
            .iter()
            .map(|capture| {
                describe_capture(
                    capture.index.0,
                    capture.node.kind_id(),
                    capture.node.byte_range(),
                )
            })
            .collect(),
    )
}

pub fn query_results(
    cursor: &mut tree_squatter::QueryCursor,
    query: &tree_squatter::Query,
    root: tree_squatter::Node<'_>,
    source: &[u8],
    captures: bool,
) -> Vec<QueryResult> {
    let mut execution = cursor.execute(query, root, source);
    let mut results = Vec::new();
    loop {
        let next = if captures {
            execution
                .next_capture()
                .map(|(found, index)| (found, Some(index)))
        } else {
            execution.next_match().map(|found| (found, None))
        };
        let Some((found, index)) = next else { break };
        results.push(query_snapshot(&found, index));
        assert!(results.len() < 100_000, "unexpected query result explosion");
    }
    assert_eq!(execution.error(), None);
    results
}

pub fn capture_set(
    results: Vec<QueryResult>,
) -> std::collections::BTreeSet<(usize, CaptureDescription)> {
    results
        .into_iter()
        .map(|(pattern, index, captures)| (pattern, captures[index.unwrap()]))
        .collect()
}
