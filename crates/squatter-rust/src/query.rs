pub use crate::query_exec::{QueryCursor, QueryExecution};
use crate::{
    Node,
    types::{CaptureId, QueryStringId},
};
use regex::bytes::Regex;
use tree_sitter::{Language, QueryPredicate, QueryPredicateArg};

#[derive(Debug)]
pub struct QueryError {
    pub offset: usize,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryExecutionError {
    UnsupportedRange,
    InvalidExecution,
}

impl std::fmt::Display for QueryExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedRange => "bounded ranges require rooted, non-branching patterns",
            Self::InvalidExecution => "query and node must use the same language",
        })
    }
}

impl std::error::Error for QueryExecutionError {}
impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "query byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for QueryError {}

#[derive(Debug)]
enum Predicate {
    EqualCapture(CaptureId, CaptureId, bool, bool),
    EqualString(CaptureId, Vec<u8>, bool, bool),
    Match(CaptureId, Regex, bool, bool),
    AnyOf(CaptureId, Vec<Vec<u8>>, bool),
}

/// Retains its language. Text predicates are compiled once; unknown predicates
/// remain available to the host through `general_predicates`.
pub struct Query {
    pub(crate) compiled: crate::native::CompiledQuery,
    pub(crate) program: crate::query_plan::Program,
    capture_names: Vec<String>,
    predicates: Vec<Vec<Predicate>>,
    general: Vec<Vec<QueryPredicate>>,
}

unsafe impl Send for Query {}
unsafe impl Sync for Query {}
impl Query {
    pub fn new(language: &Language, source: &str) -> Result<Self, QueryError> {
        let mut compiled = crate::native::CompiledQuery::new(language, source)?;
        let program = crate::query_plan::Program::new(&mut compiled);
        let mut query = Self {
            compiled,
            program,
            capture_names: Vec::new(),
            predicates: Vec::new(),
            general: Vec::new(),
        };
        for index in 0..query.compiled.view.capture_names.entries.length {
            query
                .capture_names
                .push(query.capture_name(CaptureId(index)));
        }
        for pattern in 0..query.pattern_count() {
            let range = query.compiled.patterns()[pattern].predicate_steps;
            let steps = &(unsafe { query.compiled.view.predicate_steps.as_slice() })
                [range.offset as usize..range.end()];
            let mut predicates = Vec::new();
            let mut general = Vec::new();
            let offset = query.compiled.patterns()[pattern].start_byte as usize;
            let invalid = |message: &str| QueryError {
                offset,
                message: message.into(),
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
                let capture = |index: usize| -> Result<CaptureId, QueryError> {
                    arguments
                        .get(index)
                        .filter(|step| step.kind == 1)
                        .map(|step| CaptureId(step.value_id))
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
                    _ => {
                        let args = arguments
                            .iter()
                            .map(|step| {
                                if step.kind == 1 {
                                    QueryPredicateArg::Capture(step.value_id)
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
            query.general.push(general);
        }
        Ok(query)
    }

    fn capture_name(&self, id: CaptureId) -> String {
        self.string(&self.compiled.view.capture_names, id.get() as usize)
    }

    fn string_value(&self, id: QueryStringId) -> String {
        self.string(&self.compiled.view.predicate_values, id.get() as usize)
    }

    fn string(&self, table: &crate::native::StringTable, index: usize) -> String {
        String::from_utf8(unsafe { table.get(index) }.to_vec())
            .expect("query strings originate in UTF-8")
    }

    pub fn pattern_count(&self) -> usize {
        self.compiled.patterns().len()
    }

    pub fn capture_names(&self) -> &[String] {
        &self.capture_names
    }

    pub fn general_predicates(&self, pattern: usize) -> &[QueryPredicate] {
        &self.general[pattern]
    }

    pub fn disable_pattern(&mut self, pattern: usize) {
        assert!(
            pattern < self.pattern_count(),
            "pattern index out of bounds"
        );
        self.compiled.disable_pattern(pattern as u32);
        self.program.disable_pattern(&self.compiled, pattern);
    }

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
    pub index: u32,
}
/// Captures borrow the cursor until its next advancement. Nodes borrow the tree.
/// Copy individual nodes or collect captures to retain them while advancing.
///
/// ```compile_fail
/// use tree_squatter_rust::QueryExecution;
/// fn invalid(execution: &mut QueryExecution<'_, '_, '_, '_>) {
///     let first = execution.next_match().unwrap();
///     execution.next_match();
///     println!("{}", first.captures.len()); // Still borrows the cursor.
/// }
/// ```
pub struct QueryMatch<'cursor, 'tree> {
    pub id: u32,
    pub pattern_index: usize,
    pub captures: &'cursor [QueryCapture<'tree>],
}
impl<'tree> QueryMatch<'_, 'tree> {
    pub fn nodes_for_capture_index(&self, index: u32) -> impl Iterator<Item = Node<'tree>> + '_ {
        self.nodes_for_capture(CaptureId(index))
    }

    fn nodes_for_capture(&self, id: CaptureId) -> impl Iterator<Item = Node<'tree>> + '_ {
        self.captures
            .iter()
            .filter(move |capture| capture.index == id.get())
            .map(|capture| capture.node)
    }
    pub(crate) fn satisfies(&self, query: &Query, source: &[u8]) -> bool {
        let text = |node: Node<'tree>| source.get(node.byte_range()).unwrap_or_default();
        // Preserve mainline Rust's quantifier and empty-capture behavior.
        query.predicates[self.pattern_index]
            .iter()
            .all(|predicate| match predicate {
                Predicate::EqualCapture(first, second, positive, all) => {
                    let mut left = self.nodes_for_capture(*first).peekable();
                    let mut right = self.nodes_for_capture(*second).peekable();
                    while left.peek().is_some() && right.peek().is_some() {
                        let equal = text(left.next().unwrap()) == text(right.next().unwrap());
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
                    for node in self.nodes_for_capture(*capture) {
                        let equal = text(node) == value;
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
                    for node in self.nodes_for_capture(*capture) {
                        let matches = regex.is_match(text(node));
                        if matches != *positive && *all {
                            return false;
                        }
                        if matches == *positive && !*all {
                            return true;
                        }
                    }
                    true
                }
                Predicate::AnyOf(capture, values, positive) => self
                    .nodes_for_capture(*capture)
                    .all(|node| values.iter().any(|value| value == text(node)) == *positive),
            })
    }
}
