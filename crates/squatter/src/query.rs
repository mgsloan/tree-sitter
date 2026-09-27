//! Queries over packed nodes, using Tree-sitter query syntax.
//!
//! ```
//! use tree_squatter::{Node, Query, QueryCursor, StreamingIterator};
//! # fn example(cursor: &mut QueryCursor, query: &Query, root: Node<'_>, source: &[u8]) {
//! let mut matches = cursor.matches(query, root, source);
//! while let Some(found) = matches.next() {
//!     for capture in found.captures() {
//!         let name = query.capture_names()[capture.index.0 as usize];
//!         let node = capture.node;
//!     }
//! }
//! drop(matches);
//!
//! let mut captures = cursor.captures(query, root, source);
//! while let Some((found, index)) = captures.next() {
//!     let capture = found.captures()[index.0 as usize];
//!     found.remove(); // suppress subsequent events for this match
//! }
//! # }
//! ```
//!
//! Capture events retain squatter's provisional snapshots, ordering, and
//! duplicates; they do not promise Tree-sitter's event ordering or multiplicity.
//! Use completed matches when provisional events are unsuitable. Ordinary ranges
//! intersect matched nodes and allow structural context outside the range.
//! Containing-range setters are not supported.

pub use crate::query_exec::{QueryCaptures, QueryCursor, QueryExecution, QueryMatches};
use crate::{CaptureIx, Language, MatchId, Node, PatternIx, types::QueryStringId};
use regex::bytes::Regex;
use std::cell::Cell;
pub use tree_sitter::{CaptureQuantifier, QueryErrorKind, StreamingIterator};

#[derive(Debug)]
/// A query compilation or predicate-validation error, with byte-based coordinates.
pub struct QueryError {
    pub row: usize,
    pub column: usize,
    pub offset: usize,
    pub message: String,
    pub kind: QueryErrorKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryExecutionError {
    InvalidExecution,
}

impl std::fmt::Display for QueryExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidExecution => "query and node must use the same language",
        })
    }
}

impl std::error::Error for QueryExecutionError {}
impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let prefix = match self.kind {
            QueryErrorKind::Field => "Invalid field name ",
            QueryErrorKind::NodeType => "Invalid node type ",
            QueryErrorKind::Capture => "Invalid capture name ",
            QueryErrorKind::Predicate => "Invalid predicate: ",
            QueryErrorKind::Structure => "Impossible pattern:\n",
            QueryErrorKind::Syntax => "Invalid syntax:\n",
            QueryErrorKind::Language => "",
        };
        if prefix.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(
                f,
                "Query error at {}:{}. {}{}",
                self.row + 1,
                self.column + 1,
                prefix,
                self.message
            )
        }
    }
}

impl std::error::Error for QueryError {}

#[derive(Clone, Debug)]
enum Predicate {
    EqualCapture(CaptureIx, CaptureIx, bool, bool),
    EqualString(CaptureIx, Vec<u8>, bool, bool),
    Match(CaptureIx, Regex, bool, bool),
    AnyOf(CaptureIx, Vec<Vec<u8>>, bool),
}

/// Retains its language. Text predicates are compiled once; unknown predicates
/// remain available to the host through `general_predicates`.
pub struct Query {
    pub(crate) compiled: crate::native::CompiledQuery,
    pub(crate) program: crate::query_plan::Program,
    capture_names: Box<[&'static str]>,
    quantifiers: Box<[Box<[CaptureQuantifier]>]>,
    settings: Box<[Box<[QueryProperty]>]>,
    properties: Box<[Box<[(QueryProperty, bool)]>]>,
    predicates: Vec<Vec<Predicate>>,
    general: Box<[Box<[QueryPredicate]>]>,
}

impl Query {
    /// Create a query from S-expression patterns for a prepared language.
    /// It can only run on nodes using that language; queries may be shared
    /// across threads and executions without cloning.
    pub fn new(language: &Language, source: &str) -> Result<Self, QueryError> {
        let mut compiled = crate::native::CompiledQuery::new(language, source)?;
        let program = crate::query_plan::Program::new(&mut compiled);
        let mut query = Self {
            compiled,
            program,
            capture_names: Box::default(),
            quantifiers: Box::default(),
            settings: Box::default(),
            properties: Box::default(),
            predicates: Vec::new(),
            general: Box::default(),
        };
        query.capture_names = query.borrow_capture_names();
        query.quantifiers = unsafe { query.compiled.view.capture_quantifiers.as_slice() }
            .iter()
            .map(|quantifiers| {
                let values = unsafe { quantifiers.as_slice() };
                (0..query.capture_names.len())
                    .map(|index| match values.get(index).copied().unwrap_or(0) {
                        0 => CaptureQuantifier::Zero,
                        1 => CaptureQuantifier::ZeroOrOne,
                        2 => CaptureQuantifier::ZeroOrMore,
                        3 => CaptureQuantifier::One,
                        4 => CaptureQuantifier::OneOrMore,
                        _ => unreachable!("invalid capture quantifier"),
                    })
                    .collect()
            })
            .collect();
        let mut all_general = Vec::new();
        let mut all_settings = Vec::new();
        let mut all_properties = Vec::new();
        for pattern in 0..query.pattern_count() {
            let range = query.compiled.patterns()[pattern].predicate_steps;
            let steps = &(unsafe { query.compiled.view.predicate_steps.as_slice() })
                [range.offset as usize..range.end()];
            let mut predicates = Vec::new();
            let mut general = Vec::new();
            let mut settings = Vec::new();
            let mut properties = Vec::new();
            let offset = query.compiled.patterns()[pattern].start_byte as usize;
            let invalid = |message: &str| {
                QueryError::at(source, offset, QueryErrorKind::Predicate, message.into())
            };
            for group in steps
                .split(|step| step.kind == 0)
                .filter(|group| !group.is_empty())
            {
                if group[0].kind != 2 {
                    return Err(invalid("predicate must start with a name"));
                }
                let name = query.string_value(QueryStringId(group[0].value_id));
                let arguments = &group[1..];
                let capture = |index: usize| -> Result<CaptureIx, QueryError> {
                    arguments
                        .get(index)
                        .filter(|step| step.kind == 1)
                        .map(|step| CaptureIx(step.value_id))
                        .ok_or_else(|| invalid("predicate requires a capture argument"))
                };
                let string = |index: usize| -> Result<String, QueryError> {
                    arguments
                        .get(index)
                        .filter(|step| step.kind == 2)
                        .map(|step| query.string_value(QueryStringId(step.value_id)))
                        .ok_or_else(|| invalid("predicate requires a string argument"))
                };
                match name.as_str() {
                    "eq?" | "not-eq?" | "any-eq?" | "any-not-eq?" => {
                        if arguments.len() != 2 {
                            return Err(invalid("equality predicate requires two arguments"));
                        }
                        let first = capture(0)?;
                        let positive = !name.contains("not-");
                        let all = !name.starts_with("any-");
                        predicates.push(if arguments[1].kind == 1 {
                            Predicate::EqualCapture(first, capture(1)?, positive, all)
                        } else {
                            Predicate::EqualString(first, string(1)?.into_bytes(), positive, all)
                        });
                    }
                    "match?" | "not-match?" | "any-match?" | "any-not-match?" => {
                        if arguments.len() != 2 {
                            return Err(invalid("match predicate requires two arguments"));
                        }
                        let regex =
                            Regex::new(&string(1)?).map_err(|error| invalid(&error.to_string()))?;
                        predicates.push(Predicate::Match(
                            capture(0)?,
                            regex,
                            !name.contains("not-"),
                            !name.starts_with("any-"),
                        ));
                    }
                    "any-of?" | "not-any-of?" => {
                        let first = capture(0)?;
                        let values = (1..arguments.len())
                            .map(|index| string(index).map(String::into_bytes))
                            .collect::<Result<_, _>>()?;
                        predicates.push(Predicate::AnyOf(first, values, name == "any-of?"));
                    }
                    "set!" | "is?" | "is-not?" => {
                        if arguments.is_empty() || arguments.len() > 3 {
                            return Err(invalid(
                                "property predicate requires one to three arguments",
                            ));
                        }
                        let mut capture_id = None;
                        let mut key = None;
                        let mut value = None;
                        for argument in arguments {
                            if argument.kind == 1 {
                                if capture_id.replace(CaptureIx(argument.value_id)).is_some() {
                                    return Err(invalid(
                                        "property predicate has multiple captures",
                                    ));
                                }
                            } else {
                                let text = query
                                    .string_value(QueryStringId(argument.value_id))
                                    .into_boxed_str();
                                if key.is_none() {
                                    key = Some(text);
                                } else if value.is_none() {
                                    value = Some(text);
                                } else {
                                    return Err(invalid("property predicate has too many strings"));
                                }
                            }
                        }
                        let key =
                            key.ok_or_else(|| invalid("property predicate requires a key"))?;
                        let property = QueryProperty {
                            key,
                            value,
                            capture_id,
                        };
                        if name == "set!" {
                            settings.push(property);
                        } else {
                            properties.push((property, name == "is?"));
                        }
                    }
                    _ => {
                        let args = arguments
                            .iter()
                            .map(|step| {
                                if step.kind == 1 {
                                    QueryPredicateArg::Capture(CaptureIx(step.value_id))
                                } else {
                                    QueryPredicateArg::String(
                                        query.string_value(QueryStringId(step.value_id)).into(),
                                    )
                                }
                            })
                            .collect();
                        general.push(QueryPredicate {
                            operator: name.into(),
                            args,
                        });
                    }
                }
            }
            query.predicates.push(predicates);
            all_general.push(general.into_boxed_slice());
            all_settings.push(settings.into_boxed_slice());
            all_properties.push(properties.into_boxed_slice());
        }
        query.general = all_general.into();
        query.settings = all_settings.into();
        query.properties = all_properties.into();
        Ok(query)
    }

    fn borrow_capture_names(&self) -> Box<[&'static str]> {
        (0..self.compiled.view.capture_names.entries.length as usize)
            .map(|index| {
                let bytes = unsafe { self.compiled.view.capture_names.get(index) };
                let text = std::str::from_utf8(bytes).expect("query strings originate in UTF-8");
                // The native allocation is stable until Query drops; public borrows
                // are shortened to &self. Clones rebuild these references.
                unsafe { std::mem::transmute::<&str, &'static str>(text) }
            })
            .collect()
    }

    fn string_value(&self, id: QueryStringId) -> String {
        self.string(&self.compiled.view.predicate_values, id.get() as usize)
    }

    fn string(&self, table: &crate::native::StringTable, index: usize) -> String {
        String::from_utf8(unsafe { table.get(index) }.to_vec())
            .expect("query strings originate in UTF-8")
    }

    /// Get the number of top-level patterns, including disabled patterns.
    pub fn pattern_count(&self) -> usize {
        self.compiled.patterns().len()
    }

    /// Get the query-global capture names. Disabled captures retain their indices.
    pub const fn capture_names(&self) -> &[&str] {
        &self.capture_names
    }

    /// Predicates left for host evaluation, excluding built-in text and property predicates.
    pub const fn general_predicates(&self, pattern: PatternIx) -> &[QueryPredicate] {
        &self.general[pattern.0]
    }

    /// Get the index for a given capture name.
    pub fn capture_index_for_name(&self, name: &str) -> Option<CaptureIx> {
        self.capture_names
            .iter()
            .position(|capture| *capture == name)
            .map(|index| CaptureIx(index as u32))
    }

    /// Get the quantifiers of the captures used in a pattern.
    pub const fn capture_quantifiers(&self, index: PatternIx) -> &[CaptureQuantifier] {
        &self.quantifiers[index.0]
    }

    /// Properties set by `set!` predicates, for host evaluation.
    pub const fn property_settings(&self, index: PatternIx) -> &[QueryProperty] {
        &self.settings[index.0]
    }

    /// Properties checked by `is?` (true) and `is-not?` (false), for host evaluation.
    pub const fn property_predicates(&self, index: PatternIx) -> &[(QueryProperty, bool)] {
        &self.properties[index.0]
    }

    /// Get the byte offset where the pattern starts in the query source.
    pub fn start_byte_for_pattern(&self, index: PatternIx) -> usize {
        self.compiled.patterns()[index.0].start_byte as usize
    }

    /// Get the byte offset where the pattern ends in the query source.
    pub fn end_byte_for_pattern(&self, index: PatternIx) -> usize {
        self.compiled.patterns()[index.0].end_byte as usize
    }

    /// Check whether a pattern has a single root node.
    pub fn is_pattern_rooted(&self, index: PatternIx) -> bool {
        self.compiled
            .entries()
            .iter()
            .filter(|entry| entry.pattern_index.get() as usize == index.0)
            .all(|entry| entry.flags & 1 != 0)
    }

    /// Check whether a pattern can match across repeating sibling nodes.
    pub fn is_pattern_non_local(&self, index: PatternIx) -> bool {
        self.compiled.patterns()[index.0].flags & 1 != 0
    }

    /// Check whether the step at a query-source byte offset is guaranteed to match.
    pub fn is_pattern_guaranteed_at_step(&self, offset: usize) -> bool {
        self.compiled.is_pattern_guaranteed_at_step(offset)
    }

    /// Copy the query, including disabled patterns and captures, for independent mutation.
    /// Queries can be shared across threads and cursors without cloning.
    pub fn deep_clone(&self) -> Self {
        let mut result = Self {
            compiled: self.compiled.deep_clone(),
            program: self.program.clone(),
            capture_names: Box::default(),
            quantifiers: self.quantifiers.clone(),
            settings: self.settings.clone(),
            properties: self.properties.clone(),
            predicates: self.predicates.clone(),
            general: self.general.clone(),
        };
        result.capture_names = result.borrow_capture_names();
        result
    }

    /// Disable a pattern without renumbering patterns or captures.
    pub fn disable_pattern(&mut self, pattern: PatternIx) {
        assert!(
            pattern.0 < self.pattern_count(),
            "pattern index out of bounds"
        );
        self.compiled.disable_pattern(pattern.0 as u32);
        self.program.disable_pattern(&self.compiled, pattern.0);
    }

    /// Prevent a capture from being returned or recorded during execution.
    pub fn disable_capture(&mut self, name: &str) {
        self.compiled.disable_capture(name);
        // Plans read capture IDs from the compiled steps at execution time;
        // removing an ID leaves their topology and eligibility unchanged.
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct QueryCapture<'tree> {
    pub node: Node<'tree>,
    pub index: CaptureIx,
}
/// Captures borrow the cursor until its next advancement. Nodes borrow the tree.
/// Copy individual nodes or collect captures to retain them while advancing.
///
/// ```compile_fail
/// use tree_squatter::QueryExecution;
/// fn invalid(execution: &mut QueryExecution<'_, '_, '_, &[u8], &[u8]>) {
///     let first = execution.next_match().unwrap();
///     execution.next_match();
///     println!("{}", first.captures().len()); // Still borrows the cursor.
/// }
/// ```
pub struct QueryMatch<'cursor, 'tree> {
    pub pattern_index: PatternIx,
    pub(crate) id: MatchId,
    pub(crate) captures: &'cursor [QueryCapture<'tree>],
    pub(crate) removal: &'cursor Cell<Option<MatchId>>,
}
impl<'tree> QueryMatch<'_, 'tree> {
    /// Get the match identity, which belongs to this execution only.
    pub const fn id(&self) -> MatchId {
        self.id
    }

    /// Borrow this result's captures until its next advancement.
    pub const fn captures(&self) -> &[QueryCapture<'tree>] {
        self.captures
    }

    /// Suppress subsequent results for this match. The current captures stay readable.
    /// Repeated removal is a no-op; dropping a match does not remove it.
    pub fn remove(&self) {
        self.removal.set(Some(self.id));
    }

    /// Iterate over every occurrence of a query-global capture name in this match.
    pub fn nodes_for_capture_index(
        &self,
        index: CaptureIx,
    ) -> impl Iterator<Item = Node<'tree>> + '_ {
        self.captures
            .iter()
            .filter(move |capture| capture.index == index)
            .map(|capture| capture.node)
    }

    pub(crate) fn satisfies<Provider, Chunk>(
        &self,
        query: &Query,
        provider: &mut Provider,
        buffers: &mut [Vec<u8>; 2],
    ) -> bool
    where
        Provider: TextProvider<Chunk>,
        Chunk: AsRef<[u8]>,
    {
        let [left_buffer, right_buffer] = buffers;
        // Preserve mainline Rust's quantifier and empty-capture behavior.
        query.predicates[self.pattern_index.0]
            .iter()
            .all(|predicate| match predicate {
                Predicate::EqualCapture(first, second, positive, all) => {
                    let mut left = self.nodes_for_capture_index(*first).peekable();
                    let mut right = self.nodes_for_capture_index(*second).peekable();
                    while left.peek().is_some() && right.peek().is_some() {
                        let left = capture_text(provider.text(left.next().unwrap()), left_buffer);
                        let right =
                            capture_text(provider.text(right.next().unwrap()), right_buffer);
                        let equal = left.as_ref() == right.as_ref();
                        if equal != *positive && *all {
                            return false;
                        }
                        if equal == *positive && !*all {
                            return true;
                        }
                    }
                    left.next().is_none() && right.next().is_none()
                }
                Predicate::EqualString(capture, value, positive, all) => {
                    for node in self.nodes_for_capture_index(*capture) {
                        let text = capture_text(provider.text(node), left_buffer);
                        let equal = text.as_ref() == value;
                        if equal != *positive && *all {
                            return false;
                        }
                        if equal == *positive && !*all {
                            return true;
                        }
                    }
                    true
                }
                Predicate::Match(capture, regex, positive, all) => {
                    for node in self.nodes_for_capture_index(*capture) {
                        let text = capture_text(provider.text(node), left_buffer);
                        let matches = regex.is_match(text.as_ref());
                        if matches != *positive && *all {
                            return false;
                        }
                        if matches == *positive && !*all {
                            return true;
                        }
                    }
                    true
                }
                Predicate::AnyOf(capture, values, positive) => {
                    self.nodes_for_capture_index(*capture).all(|node| {
                        let text = capture_text(provider.text(node), left_buffer);
                        values.iter().any(|value| value == text.as_ref()) == *positive
                    })
                }
            })
    }
}

/// A property set or checked by a query predicate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryProperty {
    pub key: Box<str>,
    pub value: Option<Box<str>>,
    pub capture_id: Option<CaptureIx>,
}
impl QueryProperty {
    pub fn new(key: &str, value: Option<&str>, capture_id: Option<CaptureIx>) -> Self {
        Self {
            key: key.into(),
            value: value.map(Into::into),
            capture_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryPredicateArg {
    Capture(CaptureIx),
    String(Box<str>),
}

/// A predicate left for the host to evaluate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPredicate {
    pub operator: Box<str>,
    pub args: Box<[QueryPredicateArg]>,
}

/// Supplies a node's complete text in source order. Chunks may be borrowed or
/// owned; boundaries, including boundaries inside UTF-8 sequences, are ignored.
pub trait TextProvider<Chunk: AsRef<[u8]>> {
    type I: Iterator<Item = Chunk>;
    fn text(&mut self, node: Node<'_>) -> Self::I;
}
impl<'text> TextProvider<&'text [u8]> for &'text [u8] {
    type I = std::iter::Once<&'text [u8]>;
    fn text(&mut self, node: Node<'_>) -> Self::I {
        std::iter::once(&self[node.byte_range()])
    }
}
impl<Function, Chunks, Chunk> TextProvider<Chunk> for Function
where
    Function: FnMut(Node<'_>) -> Chunks,
    Chunks: Iterator<Item = Chunk>,
    Chunk: AsRef<[u8]>,
{
    type I = Chunks;
    fn text(&mut self, node: Node<'_>) -> Self::I {
        self(node)
    }
}

enum CaptureText<'buffer, Chunk> {
    Chunk(Chunk),
    Buffer(&'buffer [u8]),
}
impl<Chunk: AsRef<[u8]>> AsRef<[u8]> for CaptureText<'_, Chunk> {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Chunk(chunk) => chunk.as_ref(),
            Self::Buffer(buffer) => buffer,
        }
    }
}
fn capture_text<Chunk: AsRef<[u8]>>(
    chunks: impl Iterator<Item = Chunk>,
    buffer: &mut Vec<u8>,
) -> CaptureText<'_, Chunk> {
    let mut chunks = chunks.filter(|chunk| !chunk.as_ref().is_empty());
    let Some(first) = chunks.next() else {
        return CaptureText::Buffer(&[]);
    };
    let Some(second) = chunks.next() else {
        return CaptureText::Chunk(first);
    };
    buffer.clear();
    buffer.extend_from_slice(first.as_ref());
    buffer.extend_from_slice(second.as_ref());
    for chunk in chunks {
        buffer.extend_from_slice(chunk.as_ref());
    }
    CaptureText::Buffer(buffer)
}

impl QueryError {
    pub(crate) fn at(source: &str, offset: usize, kind: QueryErrorKind, message: String) -> Self {
        let prefix = &source.as_bytes()[..offset];
        let row = prefix.iter().filter(|&&byte| byte == b'\n').count();
        let column = prefix
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(offset, |position| offset - position - 1);
        Self {
            row,
            column,
            offset,
            kind,
            message,
        }
    }
    pub(crate) fn compile(source: &str, offset: usize, error_type: u32) -> Self {
        let mut line_start = 0;
        let mut row = 0;
        let mut line_containing_error = None;
        for line in source.lines() {
            let line_end = line_start + line.len() + 1;
            if line_end > offset {
                line_containing_error = Some(line);
                break;
            }
            line_start = line_end;
            row += 1;
        }
        let column = offset - line_start;

        let (message, kind) = match error_type {
            // errors naming invalid tokens
            2 | 3 | 4 => {
                let suffix = source.split_at(offset).1;
                let in_quotes = offset > 0 && source.as_bytes()[offset - 1] == b'"';
                let mut backslashes = 0;
                let end_offset = suffix
                    .find(|c| {
                        if in_quotes {
                            if c == '"' && backslashes % 2 == 0 {
                                true
                            } else if c == '\\' {
                                backslashes += 1;
                                false
                            } else {
                                backslashes = 0;
                                false
                            }
                        } else {
                            !char::is_alphanumeric(c) && c != '_' && c != '-'
                        }
                    })
                    .unwrap_or(suffix.len());
                (
                    format!("\"{}\"", suffix.split_at(end_offset).0),
                    match error_type {
                        2 => QueryErrorKind::NodeType,
                        3 => QueryErrorKind::Field,
                        4 => QueryErrorKind::Capture,
                        _ => unreachable!(),
                    },
                )
            }

            // errors identifying source positions
            _ => (
                line_containing_error.map_or_else(
                    || "Unexpected EOF".to_string(),
                    |line| line.to_string() + "\n" + &" ".repeat(offset - line_start) + "^",
                ),
                match error_type {
                    5 => QueryErrorKind::Structure,
                    _ => QueryErrorKind::Syntax,
                },
            ),
        };

        QueryError {
            row,
            column,
            offset,
            message,
            kind,
        }
    }
}
