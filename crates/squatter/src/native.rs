use crate::{
    Error, FieldId, GrammarKindId, KindId, QueryError,
    types::{PatternIndex, RemappedGrammarKindId, RemappedKindId},
};
use std::{
    ffi::{CStr, c_char, c_void},
    mem::MaybeUninit,
    ptr::NonNull,
};
use tree_sitter::Language as TreeSitterLanguage;
use xxhash_rust::xxh3::Xxh3;

/// XXH3 of generated grammar tables, embedded name/version metadata, and the
/// effective name/version supplied by the caller when they are not embedded.
///
/// This does not hash the generated lexer functions or external scanner code.
/// Changes to either can change parse results without changing this hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LanguageHash(pub u64);

/// Hash the generated grammar tables and identity metadata.
pub fn language_hash(
    language: &TreeSitterLanguage,
    fallback_name: &str,
    fallback_version: Option<[u8; 3]>,
) -> LanguageHash {
    unsafe extern "C" fn visit(bytes: *const c_void, length: usize, context: *mut c_void) {
        let hasher = unsafe { &mut *context.cast::<Xxh3>() };
        let bytes = unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), length) };
        hasher.update(bytes);
    }

    let mut hasher = Xxh3::new();
    let raw = language.clone().into_raw();
    unsafe {
        sq_native_language_table_bytes(raw.cast(), visit, (&mut hasher as *mut Xxh3).cast());
        drop(TreeSitterLanguage::from_raw(raw));
    }
    match language.name() {
        Some(name) => {
            hasher.update(&[1]);
            hasher.update(&(name.len() as u64).to_le_bytes());
            hasher.update(name.as_bytes());
        }
        None => hasher.update(&[0]),
    }
    match language.metadata() {
        Some(version) => hasher.update(&[
            1,
            version.major_version,
            version.minor_version,
            version.patch_version,
        ]),
        None => hasher.update(&[0]),
    }
    let name = language.name().unwrap_or(fallback_name);
    hasher.update(&(name.len() as u64).to_le_bytes());
    hasher.update(name.as_bytes());
    let version = language.metadata().map(|metadata| {
        [
            metadata.major_version,
            metadata.minor_version,
            metadata.patch_version,
        ]
    });
    match version.or(fallback_version) {
        Some(version) => {
            hasher.update(&[1]);
            hasher.update(&version);
        }
        None => hasher.update(&[0]),
    }
    LanguageHash(hasher.digest())
}

#[repr(C)]
pub(crate) struct GrammarHandle {
    _private: [u8; 0],
}

#[repr(C)]
pub(crate) struct QueryHandle {
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
    pub symbol_count: u32,
    pub grammar_symbol_count: u32,
    pub field_count: u32,
    pub supertype_count: u32,
    pub dictionary_count: u32,
    pub dictionary_words: u32,
    pub production_fields: *const Range,
    pub direct_fields: *const u16,
    pub alias_sequences: *const u16,
    pub supertype_table: *const u32,
    pub max_alias_sequence_length: u32,
    pub supertype_table_capacity: u32,
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

    #[inline]
    pub fn remap_kind(&self, symbol: KindId) -> Option<RemappedKindId> {
        let symbol = symbol.get();
        // Values just past the public symbol table are reserved for remapped errors.
        (u32::from(symbol) < self.symbol_count || symbol >= u16::MAX - 1)
            .then(|| RemappedKindId(self.encode_id(symbol) as u16))
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

/// A tree-sitter language with shared prepared tables for packing and parsing.
///
/// Construction prepares packing tables; cloning shares them without rebuilding.
/// Direct-parser tables are prepared on first use.
pub struct Language {
    pub(crate) raw: NonNull<GrammarHandle>,
    view: NonNull<GrammarView>,
}

// Native ownership and lazy parser-table publication are atomic; published views are immutable.
unsafe impl Send for Language {}
unsafe impl Sync for Language {}
impl Clone for Language {
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

impl Drop for Language {
    fn drop(&mut self) {
        unsafe {
            sq_native_grammar_delete(self.raw.as_ptr());
        }
    }
}

impl Language {
    pub fn new(language: &TreeSitterLanguage) -> Result<Self, Error> {
        Self::create(language, None)
    }

    pub fn from_cache(language: &TreeSitterLanguage, bytes: &[u8]) -> Result<Self, Error> {
        Self::create(language, Some(bytes))
    }

    fn create(language: &TreeSitterLanguage, bytes: Option<&[u8]>) -> Result<Self, Error> {
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
        drop(unsafe { TreeSitterLanguage::from_raw(language) });
        let raw = NonNull::new(raw).ok_or_else(|| Error::from_code(error))?;
        let view = NonNull::new(unsafe { sq_native_grammar_view(raw.as_ptr()).cast_mut() })
            .expect("valid grammar has a view");
        Ok(Self { raw, view })
    }

    pub(crate) fn tables(&self) -> &GrammarView {
        unsafe { self.view.as_ref() }
    }

    pub fn tree_sitter_language(&self) -> TreeSitterLanguage {
        let borrowed = std::mem::ManuallyDrop::new(unsafe {
            TreeSitterLanguage::from_raw(self.tables().language.cast())
        });
        TreeSitterLanguage::clone(&borrowed)
    }

    /// Resolve a displayed kind name in this grammar.
    pub fn kind_id_for_name(&self, name: &str, named: bool) -> Option<KindId> {
        let language = self.tree_sitter_language();
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
        self.tree_sitter_language()
            .field_id_for_name(name)
            .map(FieldId::from)
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
        let language = language.tables().language;
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
    fn sq_native_language_table_bytes(
        language: *const c_void,
        visit: unsafe extern "C" fn(*const c_void, usize, *mut c_void),
        context: *mut c_void,
    );
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
pub(crate) struct Reduction {
    pub first_child: u32,
    pub next_sibling: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_point: Point,
    pub end_point: Point,
    pub symbol: u16,
    pub alias: u16,
    pub field: Option<FieldId>,
    pub extra: bool,
    pub visible: bool,
    pub visible_descendant_count: u32,
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
    language: Language,
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
    pub fn new(language: &Language) -> Result<Self, crate::ParseError> {
        let mut status = ParseStatus::new();
        let raw = unsafe { sq_native_parser_new(language.raw.as_ptr(), &mut status) };
        let raw = NonNull::new(raw).ok_or_else(|| status.into_error())?;

        Ok(Self {
            raw,
            language: language.clone(),
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
        // reusable. Borrowed reductions cannot outlive this guard.
        unsafe {
            sq_native_parser_clear(self.0.raw.as_ptr());
        }
    }
}

impl Reductions<'_> {
    pub fn language(&self) -> &Language {
        &self.0.language
    }

    pub fn nodes(&self) -> (&[Reduction], u32) {
        let mut count = 0;
        let mut root = 0;
        let nodes =
            unsafe { sq_native_parser_reductions(self.0.raw.as_ptr(), &mut count, &mut root) };
        // Successful parsing retains a nonempty arena until this guard drops.
        debug_assert!(root < count);
        (
            unsafe { std::slice::from_raw_parts(nodes, count as usize) },
            root,
        )
    }
}

unsafe extern "C" {
    fn sq_native_parser_new(
        language: *mut GrammarHandle,
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
    fn sq_native_parser_reductions(
        parser: *const ParserHandle,
        count: *mut u32,
        root: *mut u32,
    ) -> *const Reduction;
}
