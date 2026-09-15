//! Compiled queries and borrowed, streaming results over immutable slabs.
use crate::RawPoint;
use crate::{Node, RawNode};
use regex::bytes::Regex;
use std::{
    ffi::{c_char, c_void},
    marker::PhantomData,
    ptr::NonNull,
    time::{Duration, Instant},
};
use tree_sitter::Point;
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
    raw: NonNull<c_void>,
    capture_names: Vec<String>,
    predicates: Vec<Vec<Predicate>>,
    general: Vec<Vec<QueryPredicate>>,
}
unsafe impl Send for Query {}
unsafe impl Sync for Query {}
impl Drop for Query {
    fn drop(&mut self) {
        unsafe { ffi::sq_query_delete(self.raw.as_ptr()) }
    }
}
impl Query {
    pub fn new(language: &Language, source: &str) -> Result<Self, QueryError> {
        let length = u32::try_from(source.len()).map_err(|_| QueryError {
            offset: 0,
            message: "query exceeds u32 size".into(),
        })?;
        let mut offset = 0;
        let mut kind = 0;
        let raw_language = language.clone().into_raw();
        let pointer = unsafe {
            ffi::sq_query_new(
                raw_language.cast(),
                source.as_ptr().cast(),
                length,
                &mut offset,
                &mut kind,
            )
        };
        drop(unsafe { Language::from_raw(raw_language) });
        let raw = NonNull::new(pointer).ok_or_else(|| QueryError {
            offset: offset as usize,
            message: match kind {
                2 => "unknown node type",
                3 => "unknown field",
                4 => "unknown capture",
                5 => "invalid structure",
                6 => "incompatible language",
                _ => "invalid syntax",
            }
            .into(),
        })?;
        let mut query = Self {
            raw,
            capture_names: Vec::new(),
            predicates: Vec::new(),
            general: Vec::new(),
        };
        for index in 0..unsafe { ffi::sq_query_capture_count(raw.as_ptr()) } {
            query.capture_names.push(query.string(index, true));
        }
        for pattern in 0..query.pattern_count() {
            let mut count = 0;
            let pointer = unsafe {
                ffi::sq_query_predicates_for_pattern(raw.as_ptr(), pattern as u32, &mut count)
            };
            let steps = if count == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(pointer, count as usize) }
            };
            let mut predicates = Vec::new();
            let mut general = Vec::new();
            let offset =
                unsafe { ffi::sq_query_start_byte_for_pattern(raw.as_ptr(), pattern as u32) }
                    as usize;
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
                let name = query.string(group[0].value, false);
                let arguments = &group[1..];
                let capture = |index: usize| -> Result<u32, QueryError> {
                    arguments
                        .get(index)
                        .filter(|step| step.kind == 1)
                        .map(|step| step.value)
                        .ok_or_else(|| invalid("predicate requires a capture argument"))
                };
                let string = |index: usize| -> Result<String, QueryError> {
                    arguments
                        .get(index)
                        .filter(|step| step.kind == 2)
                        .map(|step| query.string(step.value, false))
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
                                    QueryPredicateArg::Capture(step.value)
                                } else {
                                    QueryPredicateArg::String(
                                        query.string(step.value, false).into(),
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
        let mut length = 0;
        let pointer = unsafe {
            if capture {
                ffi::sq_query_capture_name_for_id(self.raw.as_ptr(), index, &mut length)
            } else {
                ffi::sq_query_string_value_for_id(self.raw.as_ptr(), index, &mut length)
            }
        };
        if length == 0 {
            return String::new();
        }
        String::from_utf8(
            unsafe { std::slice::from_raw_parts(pointer.cast(), length as usize) }.to_vec(),
        )
        .expect("query strings originate in UTF-8")
    }
    pub fn pattern_count(&self) -> usize {
        unsafe { ffi::sq_query_pattern_count(self.raw.as_ptr()) as usize }
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
        unsafe { ffi::sq_query_disable_pattern(self.raw.as_ptr(), pattern as u32) }
    }
    pub fn disable_capture(&mut self, name: &str) {
        unsafe {
            ffi::sq_query_disable_capture(
                self.raw.as_ptr(),
                name.as_ptr().cast(),
                name.len() as u32,
            )
        }
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
/// use tree_sitter_squatter::QueryExecution;
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
        self.captures
            .iter()
            .filter(move |capture| capture.index == index)
            .map(|capture| capture.node)
    }
    fn satisfies(&self, query: &Query, source: &[u8]) -> bool {
        let text = |node: Node<'tree>| source.get(node.byte_range()).unwrap_or_default();
        // Preserve mainline Rust's quantifier and empty-capture behavior. C
        // supplies structural matches; these predicates all see the same bytes.
        query.predicates[self.pattern_index]
            .iter()
            .all(|predicate| match predicate {
                Predicate::EqualCapture(first, second, positive, all) => {
                    let mut left = self.nodes_for_capture_index(*first).peekable();
                    let mut right = self.nodes_for_capture_index(*second).peekable();
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
                    for node in self.nodes_for_capture_index(*capture) {
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
                    for node in self.nodes_for_capture_index(*capture) {
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
                    .nodes_for_capture_index(*capture)
                    .all(|node| values.iter().any(|value| value == text(node)) == *positive),
            })
    }
}

pub struct QueryCursor {
    raw: NonNull<c_void>,
    timeout: Option<Duration>,
}
unsafe impl Send for QueryCursor {}
impl Default for QueryCursor {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for QueryCursor {
    fn drop(&mut self) {
        unsafe { ffi::sq_query_cursor_delete(self.raw.as_ptr()) }
    }
}
impl QueryCursor {
    pub fn new() -> Self {
        Self {
            raw: NonNull::new(unsafe { ffi::sq_query_cursor_new() }).expect("query allocation"),
            timeout: None,
        }
    }
    pub fn set_optimized(&mut self, enabled: bool) {
        unsafe { ffi::sq_query_cursor_set_optimized(self.raw.as_ptr(), enabled) }
    }
    pub fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }
    pub fn set_match_limit(&mut self, limit: u32) {
        unsafe { ffi::sq_query_cursor_set_match_limit(self.raw.as_ptr(), limit) }
    }
    pub fn did_exceed_match_limit(&self) -> bool {
        unsafe { ffi::sq_query_cursor_did_exceed_match_limit(self.raw.as_ptr()) }
    }
    pub fn set_max_start_depth(&mut self, depth: u32) {
        unsafe { ffi::sq_query_cursor_set_max_start_depth(self.raw.as_ptr(), depth) }
    }
    pub fn set_byte_range(&mut self, range: std::ops::Range<usize>) -> bool {
        let (Ok(start), Ok(end)) = (u32::try_from(range.start), u32::try_from(range.end)) else {
            return false;
        };
        unsafe { ffi::sq_query_cursor_set_byte_range(self.raw.as_ptr(), start, end) }
    }
    pub fn set_point_range(&mut self, range: std::ops::Range<Point>) -> bool {
        let (Ok(start), Ok(end)) = (
            RawPoint::try_from(range.start),
            RawPoint::try_from(range.end),
        ) else {
            return false;
        };
        unsafe { ffi::sq_query_cursor_set_point_range(self.raw.as_ptr(), start, end) }
    }
    pub fn execute<'cursor, 'query, 'tree, 'text>(
        &'cursor mut self,
        query: &'query Query,
        node: Node<'tree>,
        source: &'text [u8],
    ) -> QueryExecution<'cursor, 'query, 'tree, 'text> {
        let mut progress = self.timeout.map(|timeout| {
            Box::new(Progress {
                started: Instant::now(),
                timeout,
                cancelled: false,
            })
        });
        let options = progress.as_mut().map(|state| {
            Box::new(Options {
                payload: (&mut **state as *mut Progress).cast(),
                callback: Some(progress_callback),
            })
        });
        unsafe {
            ffi::sq_query_cursor_exec_with_options(
                self.raw.as_ptr(),
                query.raw.as_ptr(),
                node.raw,
                options
                    .as_deref()
                    .map_or(std::ptr::null(), |options| options),
            )
        };
        QueryExecution {
            cursor: self,
            query,
            source,
            progress,
            _options: options,
            tree: PhantomData,
        }
    }
}

struct Progress {
    started: Instant,
    timeout: Duration,
    cancelled: bool,
}
#[repr(C)]
struct CursorState {
    payload: *mut c_void,
    current_byte_offset: u32,
}
#[repr(C)]
struct Options {
    payload: *mut c_void,
    callback: Option<unsafe extern "C" fn(*mut CursorState) -> bool>,
}
unsafe extern "C" fn progress_callback(state: *mut CursorState) -> bool {
    let progress = unsafe { &mut *((*state).payload.cast::<Progress>()) };
    progress.cancelled |= progress.started.elapsed() >= progress.timeout;
    progress.cancelled
}

pub struct QueryExecution<'cursor, 'query, 'tree, 'text> {
    cursor: &'cursor mut QueryCursor,
    query: &'query Query,
    source: &'text [u8],
    progress: Option<Box<Progress>>,
    // The C cursor borrows this stable allocation while execution is live.
    _options: Option<Box<Options>>,
    tree: PhantomData<&'tree crate::Tree>,
}
impl<'tree> QueryExecution<'_, '_, 'tree, '_> {
    pub fn error(&self) -> Option<QueryExecutionError> {
        match unsafe { ffi::sq_query_cursor_error(self.cursor.raw.as_ptr()) } {
            0 => None,
            1 => Some(QueryExecutionError::UnsupportedRange),
            _ => Some(QueryExecutionError::InvalidExecution),
        }
    }
    pub fn did_cancel(&self) -> bool {
        self.progress.as_ref().is_some_and(|state| state.cancelled)
    }
    pub fn remove_match(&mut self, id: u32) {
        unsafe { ffi::sq_query_cursor_remove_match(self.cursor.raw.as_ptr(), id) }
    }
    pub fn next_match(&mut self) -> Option<QueryMatch<'_, 'tree>> {
        self.next(false).map(|(result, _)| result)
    }
    pub fn next_capture(&mut self) -> Option<(QueryMatch<'_, 'tree>, usize)> {
        self.next(true)
    }
    fn next(&mut self, capture: bool) -> Option<(QueryMatch<'_, 'tree>, usize)> {
        loop {
            let mut raw = std::mem::MaybeUninit::<RawMatch>::uninit();
            let mut index = 0;
            let found = unsafe {
                if capture {
                    ffi::sq_query_cursor_next_capture(
                        self.cursor.raw.as_ptr(),
                        raw.as_mut_ptr(),
                        &mut index,
                    )
                } else {
                    ffi::sq_query_cursor_next_match(self.cursor.raw.as_ptr(), raw.as_mut_ptr())
                }
            };
            if !found {
                return None;
            }
            let raw = unsafe { raw.assume_init() };
            let captures = if raw.capture_count == 0 {
                &[]
            } else {
                unsafe {
                    std::slice::from_raw_parts(
                        raw.captures.cast::<QueryCapture<'tree>>(),
                        raw.capture_count as usize,
                    )
                }
            };
            let result = QueryMatch {
                id: raw.id,
                pattern_index: raw.pattern_index as usize,
                captures,
            };
            if result.satisfies(self.query, self.source) {
                return Some((result, index as usize));
            }
            if capture {
                unsafe { ffi::sq_query_cursor_remove_match(self.cursor.raw.as_ptr(), raw.id) }
            }
        }
    }
}

#[repr(C)]
struct PredicateStep {
    kind: i32,
    value: u32,
}
#[repr(C)]
struct RawMatch {
    id: u32,
    pattern_index: u16,
    capture_count: u16,
    captures: *const c_void,
}
mod ffi {
    use super::*;
    unsafe extern "C" {
        pub fn sq_query_new(
            language: *const c_void,
            source: *const c_char,
            length: u32,
            offset: *mut u32,
            kind: *mut i32,
        ) -> *mut c_void;
        pub fn sq_query_delete(query: *mut c_void);
        pub fn sq_query_pattern_count(query: *const c_void) -> u32;
        pub fn sq_query_capture_count(query: *const c_void) -> u32;
        pub fn sq_query_capture_name_for_id(
            query: *const c_void,
            index: u32,
            length: *mut u32,
        ) -> *const c_char;
        pub fn sq_query_string_value_for_id(
            query: *const c_void,
            index: u32,
            length: *mut u32,
        ) -> *const c_char;
        pub fn sq_query_predicates_for_pattern(
            query: *const c_void,
            pattern: u32,
            count: *mut u32,
        ) -> *const PredicateStep;
        pub fn sq_query_start_byte_for_pattern(query: *const c_void, pattern: u32) -> u32;
        pub fn sq_query_disable_capture(query: *mut c_void, name: *const c_char, length: u32);
        pub fn sq_query_disable_pattern(query: *mut c_void, pattern: u32);
        pub fn sq_query_cursor_new() -> *mut c_void;
        pub fn sq_query_cursor_delete(cursor: *mut c_void);
        pub fn sq_query_cursor_set_optimized(cursor: *mut c_void, enabled: bool);
        pub fn sq_query_cursor_exec_with_options(
            cursor: *mut c_void,
            query: *const c_void,
            node: RawNode,
            options: *const Options,
        );
        pub fn sq_query_cursor_next_match(cursor: *mut c_void, result: *mut RawMatch) -> bool;
        pub fn sq_query_cursor_next_capture(
            cursor: *mut c_void,
            result: *mut RawMatch,
            index: *mut u32,
        ) -> bool;
        pub fn sq_query_cursor_remove_match(cursor: *mut c_void, id: u32);
        pub fn sq_query_cursor_set_byte_range(cursor: *mut c_void, start: u32, end: u32) -> bool;
        pub fn sq_query_cursor_set_point_range(
            cursor: *mut c_void,
            start: RawPoint,
            end: RawPoint,
        ) -> bool;
        pub fn sq_query_cursor_set_max_start_depth(cursor: *mut c_void, depth: u32);
        pub fn sq_query_cursor_set_match_limit(cursor: *mut c_void, limit: u32);
        pub fn sq_query_cursor_did_exceed_match_limit(cursor: *const c_void) -> bool;
        pub fn sq_query_cursor_error(cursor: *const c_void) -> i32;
    }
}
