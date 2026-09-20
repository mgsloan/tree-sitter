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
    EqualCapture(u32, u32, bool, bool),
    EqualString(u32, Vec<u8>, bool, bool),
    Match(u32, Regex, bool, bool),
    AnyOf(u32, Vec<Vec<u8>>, bool),
}

/// Retains its language. Text predicates are compiled once; unknown predicates
/// remain available to the host through `general_predicates`.
pub struct Query {
    pub(crate) compiled: crate::native::CompiledQuery,
    capture_names: Vec<String>,
    predicates: Vec<Vec<Predicate>>,
    general: Vec<Vec<QueryPredicate>>,
}
unsafe impl Send for Query {}
unsafe impl Sync for Query {}
impl Query {
    pub fn new(language: &Language, source: &str) -> Result<Self, QueryError> {
        let compiled = crate::native::CompiledQuery::new(language, source)?;
        let mut query = Self {
            compiled,
            capture_names: Vec::new(),
            predicates: Vec::new(),
            general: Vec::new(),
        };
        for index in 0..query.compiled.view.capture_names.entries.length {
            query.capture_names.push(query.string(index, true));
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
                let name = query.string(group[0].value_id, false);
                let arguments = &group[1..];
                let capture = |index: usize| -> Result<u32, QueryError> {
                    arguments
                        .get(index)
                        .filter(|step| step.kind == 1)
                        .map(|step| step.value_id)
                        .ok_or_else(|| invalid("predicate requires a capture argument"))
                };
                let string = |index: usize| -> Result<String, QueryError> {
                    arguments
                        .get(index)
                        .filter(|step| step.kind == 2)
                        .map(|step| query.string(step.value_id, false))
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
                                        query.string(step.value_id, false).into(),
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
    fn string(&self, index: u32, capture: bool) -> String {
        let table = if capture {
            &self.compiled.view.capture_names
        } else {
            &self.compiled.view.predicate_values
        };
        String::from_utf8(unsafe { table.get(index as usize) }.to_vec())
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
    }
    pub fn disable_capture(&mut self, name: &str) {
        self.compiled.disable_capture(name);
    }
}
