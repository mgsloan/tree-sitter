use crate::{
    Error, FieldId, GrammarKindId, KindId, QueryError,
    types::{PatternIndex, RemappedGrammarKindId, RemappedKindId, SymbolCode},
};
use std::{
    ffi::{CStr, c_char, c_void},
    mem::MaybeUninit,
    ptr::NonNull,
};
use tree_sitter::Language;

#[repr(C)]
pub(crate) struct GrammarHandle {
    _private: [u8; 0],
}

#[repr(C)]
pub(crate) struct QueryHandle {
    _private: [u8; 0],
}

#[repr(C)]
pub(crate) struct TraversalHandle {
    _private: [u8; 0],
}

#[repr(C)]
pub(crate) struct ParserHandle {
    _private: [u8; 0],
}

#[repr(C)]
pub(crate) struct GrammarView {
    pub language: *const c_void,
    pub symbol_names: *const *const c_char,
    pub field_names: *const *const c_char,
    pub symbol_flags: *const u8,
    pub public_symbols: *const u16,
    pub supertypes: *const u16,
    pub supertype_indexes: *const u16,
    pub supertype_masks: *const u64,
    pub grammar_ids: *const u16,
    pub default_codes: *const u16,
    pub counts: *const u16,
    pub defaults: *const u16,
    pub grammar_codes: *const u16,
    pub symbol_count: u32,
    pub grammar_symbol_count: u32,
    pub field_count: u32,
    pub supertype_count: u32,
    pub dictionary_count: u32,
    pub dictionary_words: u32,
    pub encoding: u32,
    pub dictionary_length: u32,
    pub symbol_shift: u8,
    pub separate: u8,
}

impl GrammarView {
    pub fn supertypes(&self) -> &[u16] {
        if self.supertype_count == 0 {
            return &[];
        }
        // The native owner retains these immutable arrays with the view.
        unsafe { std::slice::from_raw_parts(self.supertypes, self.supertype_count as usize) }
    }

    pub fn supertype_masks(&self) -> &[u64] {
        let length = self.dictionary_count as usize * self.dictionary_words as usize;
        if length == 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.supertype_masks, length) }
    }

    pub fn symbol_code(
        &self,
        display: RemappedKindId,
        original: RemappedGrammarKindId,
    ) -> Option<SymbolCode> {
        self.symbol_code_raw(display.get(), original.get())
            .map(SymbolCode)
    }

    fn symbol_code_raw(&self, display: u16, original: u16) -> Option<u16> {
        unsafe {
            if *self.public_symbols.add(original as usize) == display {
                return Some(*self.default_codes.add(original as usize));
            }
            if self.separate != 0 {
                return Some(display);
            }
            if self.encoding == 2 {
                return Some((display << 8) | original);
            }
            let count = *self.counts.add(display as usize);
            if self.encoding == 1 {
                let variant = if count == 1 {
                    0
                } else {
                    *self.grammar_codes.add(original as usize)
                };
                if variant == 0 && (count != 1 || *self.defaults.add(display as usize) != original)
                {
                    return None;
                }
                return Some((display << self.symbol_shift) | variant);
            }
            let start = display << self.symbol_shift;
            (0..count)
                .find(|variant| *self.grammar_ids.add((start + variant) as usize) == original)
                .map(|variant| start + variant)
        }
    }

    #[inline]
    pub fn remap_kind(&self, symbol: KindId) -> RemappedKindId {
        RemappedKindId(self.encode_id(symbol.get()) as u16)
    }

    pub fn decode_kind(&self, symbol: RemappedKindId) -> KindId {
        KindId::new(self.decode_id(symbol.get() as u32))
    }

    pub fn decode_grammar_kind(&self, symbol: RemappedGrammarKindId) -> GrammarKindId {
        GrammarKindId::new(self.decode_id(symbol.get() as u32))
    }

    fn encode_id(&self, symbol: u16) -> u32 {
        match symbol {
            u16::MAX => self.symbol_count,
            65534 => self.symbol_count + 1,
            _ => symbol as u32,
        }
    }

    #[inline]
    fn decode_id(&self, index: u32) -> u16 {
        if index == self.symbol_count {
            u16::MAX
        } else if index == self.symbol_count + 1 {
            65534
        } else {
            index as u16
        }
    }

    pub fn symbol_name(&self, symbol: u16) -> &str {
        match symbol {
            u16::MAX => "ERROR",
            65534 => "_ERROR",
            _ => unsafe {
                CStr::from_ptr(*self.symbol_names.add(symbol as usize))
                    .to_str()
                    .unwrap()
            },
        }
    }

    pub fn field_name(&self, field: u16) -> Option<&str> {
        if field == 0 || field as u32 > self.field_count {
            return None;
        }
        Some(unsafe {
            CStr::from_ptr(*self.field_names.add(field as usize))
                .to_str()
                .unwrap()
        })
    }

    #[inline]
    pub fn named(&self, symbol: u16) -> bool {
        unsafe { *self.symbol_flags.add(self.encode_id(symbol) as usize) & 1 != 0 }
    }
}

pub struct Grammar {
    pub(crate) raw: NonNull<GrammarHandle>,
    view: NonNull<GrammarView>,
}

// Native ownership and lazy parser-table publication are atomic; published views are immutable.
unsafe impl Send for Grammar {}
unsafe impl Sync for Grammar {}
impl Clone for Grammar {
    fn clone(&self) -> Self {
        unsafe {
            sq_native_grammar_copy(self.raw.as_ptr());
        }
        Self {
            raw: self.raw,
            view: self.view,
        }
    }
}

impl Drop for Grammar {
    fn drop(&mut self) {
        unsafe {
            sq_native_grammar_delete(self.raw.as_ptr());
        }
    }
}

impl Grammar {
    pub fn new(language: &Language) -> Result<Self, Error> {
        Self::create(language, None)
    }

    pub fn from_cache(language: &Language, bytes: &[u8]) -> Result<Self, Error> {
        Self::create(language, Some(bytes))
    }

    fn create(language: &Language, bytes: Option<&[u8]>) -> Result<Self, Error> {
        let language = language.clone().into_raw();
        let mut error = 0;
        let raw = unsafe {
            match bytes {
                Some(bytes) => sq_native_grammar_new_with_cache(
                    language.cast(),
                    bytes.as_ptr(),
                    bytes.len(),
                    &mut error,
                ),
                None => sq_native_grammar_new(language.cast(), &mut error),
            }
        };
        drop(unsafe { Language::from_raw(language) });
        let raw = NonNull::new(raw).ok_or_else(|| Error::from_code(error))?;
        let view =
            unsafe { NonNull::new_unchecked(sq_native_grammar_view(raw.as_ptr()).cast_mut()) };
        Ok(Self { raw, view })
    }

    pub(crate) fn tables(&self) -> &GrammarView {
        unsafe { self.view.as_ref() }
    }

    pub fn language(&self) -> Language {
        let borrowed = std::mem::ManuallyDrop::new(unsafe {
            Language::from_raw(self.tables().language.cast())
        });
        Language::clone(&borrowed)
    }

    /// Resolve a displayed kind name in this grammar.
    pub fn kind_id_for_name(&self, name: &str, named: bool) -> Option<KindId> {
        let language = self.language();
        let id = language.id_for_node_kind(name, named);
        (language.node_kind_for_id(id) == Some(name) && self.tables().named(id) == named)
            .then_some(KindId::new(id))
    }

    /// Resolve an original grammar kind, including kinds hidden by aliases.
    pub fn grammar_kind_id_for_name(&self, name: &str, named: bool) -> Option<GrammarKindId> {
        let tables = self.tables();
        (0..tables.grammar_symbol_count as u16)
            .chain([u16::MAX, u16::MAX - 1])
            .find_map(|id| {
                (tables.symbol_name(id) == name && tables.named(id) == named)
                    .then_some(GrammarKindId::new(id))
            })
    }

    /// Resolve a field name in this grammar.
    pub fn field_id_for_name(&self, name: &str) -> Option<FieldId> {
        self.language().field_id_for_name(name).map(FieldId::from)
    }

    pub fn cache(&self) -> Result<Vec<u8>, Error> {
        let length = unsafe { sq_native_grammar_cache_size(self.raw.as_ptr()) } as usize;
        let mut bytes = Vec::<u8>::with_capacity(length);
        if length != 0 {
            let mut error = 0;
            if !unsafe {
                sq_native_grammar_copy_cache(
                    self.raw.as_ptr(),
                    bytes.as_mut_ptr(),
                    length,
                    &mut error,
                )
            } {
                return Err(Error::from_code(error));
            }
            unsafe {
                bytes.set_len(length);
            }
        }
        Ok(bytes)
    }
}

#[repr(C)]
pub(crate) struct NativeSlice<T> {
    pub data: *const T,
    pub length: u32,
}
impl<T> NativeSlice<T> {
    // The caller retains the native allocation and excludes mutation for the returned borrow.
    pub unsafe fn as_slice(&self) -> &[T] {
        if self.length == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.data, self.length as usize) }
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Range {
    pub offset: u32,
    pub length: u32,
}

impl Range {
    pub fn end(self) -> usize {
        self.offset as usize + self.length as usize
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Step {
    pub symbol: u16,
    pub supertype_symbol: u16,
    pub field: u16,
    pub capture_ids: [u16; 3],
    pub depth: u16,
    pub alternative_index: u16,
    pub negated_field_list_id: u16,
    pub flags: u16,
}
pub(crate) mod flags {
    include!(concat!(env!("OUT_DIR"), "/query_flags.rs"));
}

impl Step {
    #[inline]
    pub fn has(&self, flag: u16) -> bool {
        self.flags & flag != 0
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct PatternEntry {
    pub step_index: u16,
    pub pattern_index: PatternIndex,
    pub presence_requirement: u16,
    pub flags: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pattern {
    pub steps: Range,
    pub predicate_steps: Range,
    pub start_byte: u32,
    pub end_byte: u32,
    pub flags: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct PredicateStep {
    pub kind: u32,
    pub value_id: u32,
}

#[repr(C)]
pub(crate) struct StringTable {
    pub bytes: NativeSlice<u8>,
    pub entries: NativeSlice<Range>,
}

impl StringTable {
    pub unsafe fn get(&self, index: usize) -> &[u8] {
        let entry = unsafe { self.entries.as_slice()[index] };
        &(unsafe { self.bytes.as_slice() })[entry.offset as usize..entry.end()]
    }
}

#[repr(C)]
pub(crate) struct QueryView {
    pub language: *const c_void,
    pub symbol_count: u32,
    pub public_symbols: NativeSlice<u16>,
    pub steps: NativeSlice<Step>,
    pub pattern_entries: NativeSlice<PatternEntry>,
    pub patterns: NativeSlice<Pattern>,
    pub predicate_steps: NativeSlice<PredicateStep>,
    pub capture_names: StringTable,
    pub predicate_values: StringTable,
    pub capture_quantifiers: NativeSlice<NativeSlice<u8>>,
    pub negated_fields: NativeSlice<u16>,
    pub rootless_repeat_symbols: NativeSlice<u16>,
    pub wildcard_root_pattern_count: u32,
}
const _: () = {
    assert!(size_of::<Step>() == 20);
    assert!(std::mem::offset_of!(Step, flags) == 18);
    assert!(size_of::<PatternEntry>() == 8);
    assert!(size_of::<Pattern>() == 28);
};

pub(crate) struct CompiledQuery {
    // The view borrows this allocation. Mutation requires exclusive access and
    // refreshes the view because native arrays may move.
    raw: NonNull<QueryHandle>,
    pub view: QueryView,
}

unsafe impl Send for CompiledQuery {}
unsafe impl Sync for CompiledQuery {}
impl Drop for CompiledQuery {
    fn drop(&mut self) {
        unsafe {
            sq_native_query_delete(self.raw.as_ptr());
        }
    }
}

impl CompiledQuery {
    pub fn new(language: &Language, source: &str) -> Result<Self, QueryError> {
        let length = u32::try_from(source.len()).map_err(|_| QueryError {
            offset: 0,
            message: "query exceeds u32 size".into(),
        })?;
        let language = language.clone().into_raw();
        let mut offset = 0;
        let mut kind = 0;
        let raw = unsafe {
            sq_native_query_new(
                language.cast(),
                source.as_ptr(),
                length,
                &mut offset,
                &mut kind,
            )
        };
        drop(unsafe { Language::from_raw(language) });
        let raw = NonNull::new(raw).ok_or_else(|| QueryError {
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
        let mut view = MaybeUninit::uninit();
        unsafe {
            sq_native_query_view(raw.as_ptr(), view.as_mut_ptr());
        }
        let result = Self {
            raw,
            view: unsafe { view.assume_init() },
        };
        #[cfg(debug_assertions)]
        result.validate();
        Ok(result)
    }

    pub fn steps(&self) -> &[Step] {
        unsafe { self.view.steps.as_slice() }
    }

    pub fn entries(&self) -> &[PatternEntry] {
        unsafe { self.view.pattern_entries.as_slice() }
    }

    pub fn patterns(&self) -> &[Pattern] {
        unsafe { self.view.patterns.as_slice() }
    }

    pub fn steps_mut(&mut self) -> &mut [Step] {
        // These records remain C-owned, but no native code accesses them while
        // Rust holds this exclusive borrow. Drop still uses the C allocator.
        if self.view.steps.length == 0 {
            &mut []
        } else {
            unsafe {
                std::slice::from_raw_parts_mut(
                    self.view.steps.data.cast_mut(),
                    self.view.steps.length as usize,
                )
            }
        }
    }

    pub fn entries_mut(&mut self) -> &mut [PatternEntry] {
        if self.view.pattern_entries.length == 0 {
            &mut []
        } else {
            unsafe {
                std::slice::from_raw_parts_mut(
                    self.view.pattern_entries.data.cast_mut(),
                    self.view.pattern_entries.length as usize,
                )
            }
        }
    }

    pub fn disable_pattern(&mut self, pattern: u32) {
        unsafe {
            sq_native_query_disable_pattern(self.raw.as_ptr(), pattern);
            self.refresh();
        }
    }

    pub fn disable_capture(&mut self, name: &str) {
        let Ok(length) = u32::try_from(name.len()) else {
            return;
        };
        unsafe {
            sq_native_query_disable_capture(self.raw.as_ptr(), name.as_ptr(), length);
        }
        // Capture removal only edits step records; no arrays move or resize.
        #[cfg(debug_assertions)]
        self.validate();
    }

    unsafe fn refresh(&mut self) {
        unsafe {
            sq_native_query_view(self.raw.as_ptr(), &mut self.view);
        }
        #[cfg(debug_assertions)]
        self.validate();
    }

    #[cfg(debug_assertions)]
    fn validate(&self) {
        let steps = self.steps();
        for step in steps {
            assert_eq!(step.flags & !0x0fff, 0);
            assert!(
                step.alternative_index == u16::MAX
                    || (step.alternative_index as usize) < steps.len()
            );
            assert!((step.negated_field_list_id as u32) < self.view.negated_fields.length);
            for capture in step.capture_ids {
                assert!(
                    capture == u16::MAX
                        || (capture as u32) < self.view.capture_names.entries.length
                );
            }
        }
        for entry in self.entries() {
            assert!((entry.step_index as usize) < steps.len());
            assert!((entry.pattern_index.get() as usize) < self.patterns().len());
            assert_eq!(entry.flags & !1, 0);
        }
        for pattern in self.patterns() {
            assert!(pattern.steps.end() <= steps.len());
            assert!(pattern.predicate_steps.end() <= self.view.predicate_steps.length as usize);
            assert_eq!(pattern.flags & !1, 0);
        }
        for table in [&self.view.capture_names, &self.view.predicate_values] {
            for entry in unsafe { table.entries.as_slice() } {
                assert!(entry.end() <= table.bytes.length as usize);
            }
        }
        for predicate in unsafe { self.view.predicate_steps.as_slice() } {
            match predicate.kind {
                0 => {}
                1 => assert!(predicate.value_id < self.view.capture_names.entries.length),
                2 => assert!(predicate.value_id < self.view.predicate_values.entries.length),
                _ => panic!("invalid predicate kind"),
            }
        }
        assert_eq!(
            self.view.capture_quantifiers.length as usize,
            self.patterns().len()
        );
        for quantifiers in unsafe { self.view.capture_quantifiers.as_slice() } {
            assert!(quantifiers.length <= self.view.capture_names.entries.length);
            assert!(
                unsafe { quantifiers.as_slice() }
                    .iter()
                    .all(|quantifier| *quantifier <= 4)
            );
        }
    }
}

unsafe extern "C" {
    fn sq_native_grammar_new(language: *const c_void, error: *mut i32) -> *mut GrammarHandle;
    fn sq_native_grammar_new_with_cache(
        language: *const c_void,
        bytes: *const u8,
        length: usize,
        error: *mut i32,
    ) -> *mut GrammarHandle;
    fn sq_native_grammar_copy(grammar: *mut GrammarHandle) -> *mut GrammarHandle;
    fn sq_native_grammar_delete(grammar: *mut GrammarHandle);
    fn sq_native_grammar_view(grammar: *const GrammarHandle) -> *const GrammarView;
    fn sq_native_grammar_cache_size(grammar: *const GrammarHandle) -> u32;
    fn sq_native_grammar_copy_cache(
        grammar: *const GrammarHandle,
        destination: *mut u8,
        length: usize,
        error: *mut i32,
    ) -> bool;
    fn sq_native_query_new(
        language: *const c_void,
        source: *const u8,
        length: u32,
        offset: *mut u32,
        kind: *mut u32,
    ) -> *mut QueryHandle;
    fn sq_native_query_delete(query: *mut QueryHandle);
    fn sq_native_query_view(query: *const QueryHandle, view: *mut QueryView);
    fn sq_native_query_disable_pattern(query: *mut QueryHandle, pattern: u32);
    fn sq_native_query_disable_capture(query: *mut QueryHandle, name: *const u8, length: u32);
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Point {
    pub row: u32,
    pub column: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Event {
    pub depth: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_point: Point,
    pub end_point: Point,
    pub symbol: RemappedKindId,
    pub grammar: RemappedGrammarKindId,
    pub field: Option<FieldId>,
    pub supertype: u16,
    pub flags: u16,
}
const _: () = assert!(size_of::<Event>() == 40);

pub(crate) struct Traversal(NonNull<TraversalHandle>);
unsafe impl Send for Traversal {}
impl Drop for Traversal {
    fn drop(&mut self) {
        unsafe {
            sq_native_traversal_delete(self.0.as_ptr());
        }
    }
}
pub(crate) struct Events<'input> {
    traversal: &'input mut Traversal,
    // Native frames borrow tree/reduction storage between refills.
    input: std::marker::PhantomData<&'input ()>,
}

impl Drop for Events<'_> {
    fn drop(&mut self) {
        unsafe {
            sq_native_traversal_end(self.traversal.0.as_ptr());
        }
    }
}

impl Traversal {
    pub fn new() -> Result<Self, Error> {
        NonNull::new(unsafe { sq_native_traversal_new() })
            .map(Self)
            .ok_or(Error::Allocation)
    }

    pub fn trim(&mut self) {
        unsafe {
            sq_native_traversal_trim(self.0.as_ptr());
        }
    }

    pub fn tree<'input>(
        &'input mut self,
        grammar: &'input Grammar,
        tree: &'input tree_sitter::Tree,
        points: bool,
    ) -> Result<Events<'input>, Error> {
        let mut error = 0;
        if !unsafe {
            sq_native_traversal_begin_tree(
                self.0.as_ptr(),
                grammar.raw.as_ptr(),
                tree.root_node().into_raw().tree.cast(),
                points,
                &mut error,
            )
        } {
            return Err(Error::from_code(error));
        }
        Ok(Events {
            traversal: self,
            input: std::marker::PhantomData,
        })
    }
}

impl Events<'_> {
    pub fn expected_nodes(&self) -> u32 {
        unsafe { sq_native_traversal_node_count(self.traversal.0.as_ptr()) }
    }

    pub fn fill<'buffer>(
        &mut self,
        buffer: &'buffer mut [MaybeUninit<Event>],
    ) -> Result<(&'buffer [Event], bool), Error> {
        let capacity = u32::try_from(buffer.len()).map_err(|_| Error::Overflow)?;
        let mut written = 0;
        let mut done = false;
        let mut error = 0;
        if !unsafe {
            sq_native_traversal_fill(
                self.traversal.0.as_ptr(),
                buffer.as_mut_ptr().cast(),
                capacity,
                &mut written,
                &mut done,
                &mut error,
            )
        } {
            return Err(Error::from_code(error));
        }
        debug_assert!(written <= capacity && (written != 0 || done));
        Ok((
            unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast(), written as usize) },
            done,
        ))
    }
}

unsafe extern "C" {
    fn sq_native_traversal_new() -> *mut TraversalHandle;
    fn sq_native_traversal_delete(traversal: *mut TraversalHandle);
    fn sq_native_traversal_trim(traversal: *mut TraversalHandle);
    fn sq_native_traversal_end(traversal: *mut TraversalHandle);
    fn sq_native_traversal_node_count(traversal: *const TraversalHandle) -> u32;
    fn sq_native_traversal_begin_tree(
        traversal: *mut TraversalHandle,
        grammar: *mut GrammarHandle,
        tree: *const c_void,
        points: bool,
        error: *mut i32,
    ) -> bool;
    fn sq_native_traversal_fill(
        traversal: *mut TraversalHandle,
        events: *mut Event,
        capacity: u32,
        written: *mut u32,
        done: *mut bool,
        error: *mut i32,
    ) -> bool;
}

#[repr(C)]
struct ParseStatus {
    code: i32,
    byte: u32,
    point: Point,
    message: [u8; 512],
}

impl ParseStatus {
    fn new() -> Self {
        Self {
            code: 0,
            byte: 0,
            point: Point::default(),
            message: [0; 512],
        }
    }

    fn into_error(self) -> crate::ParseError {
        let length = self
            .message
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(512);
        crate::ParseError {
            code: Error::from_code(self.code),
            byte: self.byte,
            point: tree_sitter::Point::new(self.point.row as usize, self.point.column as usize),
            message: String::from_utf8_lossy(&self.message[..length]).into_owned(),
        }
    }
}

pub(crate) struct NativeParser {
    raw: NonNull<ParserHandle>,
    grammar: Grammar,
}

unsafe impl Send for NativeParser {}

impl Drop for NativeParser {
    fn drop(&mut self) {
        unsafe {
            sq_native_parser_delete(self.raw.as_ptr());
        }
    }
}

impl NativeParser {
    pub fn new(grammar: &Grammar) -> Result<Self, crate::ParseError> {
        let mut status = ParseStatus::new();
        let raw = unsafe { sq_native_parser_new(grammar.raw.as_ptr(), &mut status) };
        let raw = NonNull::new(raw).ok_or_else(|| status.into_error())?;

        Ok(Self {
            raw,
            grammar: grammar.clone(),
        })
    }

    pub fn parse(&mut self, source: &[u8]) -> Result<Reductions<'_>, crate::ParseError> {
        let length = u32::try_from(source.len()).map_err(|_| crate::ParseError {
            code: Error::Overflow,
            byte: 0,
            point: tree_sitter::Point::default(),
            message: "source exceeds the 32-bit byte limit".into(),
        })?;
        let mut status = ParseStatus::new();
        if !unsafe {
            sq_native_parser_parse(self.raw.as_ptr(), source.as_ptr(), length, &mut status)
        } {
            return Err(status.into_error());
        }

        Ok(Reductions(self))
    }

    pub fn trim(&mut self) {
        unsafe {
            sq_native_parser_trim(self.raw.as_ptr());
        }
    }
}

pub(crate) struct Reductions<'parse>(&'parse mut NativeParser);

impl Drop for Reductions<'_> {
    fn drop(&mut self) {
        // Clear logical state even if encoding fails or unwinds; capacity stays
        // reusable. Events borrow this guard and must end before it is dropped.
        unsafe {
            sq_native_parser_clear(self.0.raw.as_ptr());
        }
    }
}

impl Reductions<'_> {
    pub fn grammar(&self) -> &Grammar {
        &self.0.grammar
    }

    pub fn events<'input>(
        &'input self,
        traversal: &'input mut Traversal,
        points: bool,
    ) -> Result<Events<'input>, Error> {
        let mut error = 0;
        if !unsafe {
            sq_native_parser_begin(
                self.0.raw.as_ptr(),
                traversal.0.as_ptr(),
                points,
                &mut error,
            )
        } {
            return Err(Error::from_code(error));
        }

        Ok(Events {
            traversal,
            input: std::marker::PhantomData,
        })
    }
}

unsafe extern "C" {
    fn sq_native_parser_new(
        grammar: *mut GrammarHandle,
        error: *mut ParseStatus,
    ) -> *mut ParserHandle;
    fn sq_native_parser_delete(parser: *mut ParserHandle);
    fn sq_native_parser_trim(parser: *mut ParserHandle);
    fn sq_native_parser_clear(parser: *mut ParserHandle);
    fn sq_native_parser_parse(
        parser: *mut ParserHandle,
        source: *const u8,
        length: u32,
        error: *mut ParseStatus,
    ) -> bool;
    fn sq_native_parser_begin(
        parser: *mut ParserHandle,
        traversal: *mut TraversalHandle,
        points: bool,
        error: *mut i32,
    ) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_preserve_visible_nodes_across_refills() {
        let languages = [
            (
                unsafe { Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) },
                "{\"a\": [1, true, {\"b\": null}], \"c\": []}",
            ),
            (
                unsafe { Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) },
                "int f(int x) { /* extra */ if (x) return x + 1; return 0; }",
            ),
        ];
        for (language, source) in languages {
            let grammar = Grammar::new(&language).unwrap();
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&language).unwrap();
            let tree = parser.parse(source, None).unwrap();
            let mut expected = Vec::new();
            let mut cursor = tree.walk();
            let mut depth = 0;
            loop {
                expected.push((
                    cursor.node(),
                    depth,
                    cursor.field_id().map_or(0, |field| field.get()),
                ));
                if cursor.goto_first_child() {
                    depth += 1;
                    continue;
                }
                loop {
                    if cursor.goto_next_sibling() {
                        break;
                    }
                    if !cursor.goto_parent() {
                        break;
                    }
                    depth -= 1;
                }
                if depth == 0 {
                    break;
                }
            }
            let mut traversal = Traversal::new().unwrap();
            for capacity in [1, 2, 7, 128] {
                for points in [false, true] {
                    let mut input = traversal.tree(&grammar, &tree, points).unwrap();
                    let mut buffer = vec![MaybeUninit::uninit(); capacity];
                    let mut actual = Vec::new();
                    loop {
                        let (batch, done) = input.fill(&mut buffer).unwrap();
                        actual.extend_from_slice(batch);
                        if done {
                            break;
                        }
                    }
                    assert_eq!(actual.len(), expected.len());
                    for (event, (node, depth, field)) in actual.iter().zip(expected.iter().rev()) {
                        assert_eq!(event.depth, *depth);
                        assert_eq!(
                            grammar.tables().decode_kind(event.symbol).get(),
                            node.kind_id()
                        );
                        assert_eq!(
                            grammar.tables().decode_grammar_kind(event.grammar).get(),
                            node.grammar_id()
                        );
                        assert_eq!(event.field.map_or(0, FieldId::get), *field);
                        assert_eq!(event.start_byte as usize, node.start_byte());
                        assert_eq!(event.end_byte as usize, node.end_byte());
                        assert_eq!(event.flags & 2 != 0, node.is_extra());
                        assert_eq!(event.flags & 4 != 0, node.is_missing());
                        assert_eq!(event.flags & 8 != 0, node.has_error());
                        if points {
                            assert_eq!(event.start_point.row as usize, node.start_position().row);
                            assert_eq!(
                                event.start_point.column as usize,
                                node.start_position().column
                            );
                            assert_eq!(event.end_point.row as usize, node.end_position().row);
                            assert_eq!(event.end_point.column as usize, node.end_position().column);
                        }
                    }
                }
            }
        }
    }
}
