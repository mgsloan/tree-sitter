use crate::{
    Error, FieldId, GrammarId, KindId, QueryError,
    types::{PatternIndex, SquatterGrammarId, SquatterKindId},
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
pub struct LanguageHash(u64);

impl LanguageHash {
    pub const fn from_raw_digest(value: u64) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

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
    LanguageHash::from_raw_digest(hasher.digest())
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
    pub kind_to_native: *const u16,
    pub native_to_kind: *const u16,
    pub grammar_to_native: *const u16,
    pub native_to_grammar: *const u16,
    pub default_grammar: *const u16,
    pub kind_flags: *const u8,
    pub kind_count: u32,
    pub compact_grammar_count: u32,
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
    pub fn remap_kind(&self, symbol: KindId) -> Option<SquatterKindId> {
        if u32::from(symbol.raw()) >= self.symbol_count && symbol.raw() < u16::MAX - 1 {
            return None;
        }
        let index = self.native_index(symbol.raw());
        if index >= self.symbol_count + 2 {
            return None;
        }
        let id = unsafe { *self.native_to_kind.add(index as usize) };
        (id != 0).then_some(SquatterKindId(id))
    }

    pub fn remap_grammar(&self, symbol: GrammarId) -> Option<SquatterGrammarId> {
        if u32::from(symbol.raw()) >= self.symbol_count && symbol.raw() < u16::MAX - 1 {
            return None;
        }
        let index = self.native_index(symbol.raw());
        if index >= self.symbol_count + 2 {
            return None;
        }
        let id = unsafe { *self.native_to_grammar.add(index as usize) };
        (id != 0).then_some(SquatterGrammarId(id))
    }

    pub fn decode_kind(&self, symbol: SquatterKindId) -> KindId {
        KindId::from_raw(unsafe { *self.kind_to_native.add(symbol.raw() as usize) })
    }

    pub fn decode_grammar_kind(&self, symbol: SquatterGrammarId) -> GrammarId {
        GrammarId::from_raw(unsafe { *self.grammar_to_native.add(symbol.raw() as usize) })
    }

    fn native_index(&self, symbol: u16) -> u32 {
        match symbol {
            u16::MAX => self.symbol_count,
            65534 => self.symbol_count + 1,
            _ => symbol as u32,
        }
    }

    #[inline]
    pub fn default_grammar(&self, symbol: SquatterKindId) -> SquatterGrammarId {
        SquatterGrammarId(unsafe { *self.default_grammar.add(symbol.raw() as usize) })
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
        unsafe { *self.symbol_flags.add(self.native_index(symbol) as usize) & 1 != 0 }
    }

    #[inline]
    pub fn named_index(&self, symbol: SquatterKindId) -> bool {
        unsafe { *self.kind_flags.add(symbol.raw() as usize) & 1 != 0 }
    }
}

/// An opaque object that defines how to parse a particular language. The code
/// for each `Language` is generated by the Tree-sitter CLI.
///
/// **Different than Tree-sitter:** Wraps a tree-sitter language with shared prepared
/// packing tables. Construction prepares those tables; cloning shares them. Direct-parser
/// tables are prepared on first use.
pub struct Language {
    language: TreeSitterLanguage,
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
            language: self.language.clone(),
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
    /// Prepares packing metadata for the underlying grammar.
    /// Unsupported metadata or representation limits return an error.
    ///
    /// **Not in Tree-sitter**
    pub fn new(language: &TreeSitterLanguage) -> Result<Self, Error> {
        Self::create(language, None)
    }

    /// Prepares a grammar using previously persisted grammar-cache
    /// bytes. Invalid or incompatible caches return an error.
    ///
    /// **Not in Tree-sitter**
    pub fn from_cache(language: &TreeSitterLanguage, bytes: &[u8]) -> Result<Self, Error> {
        Self::create(language, Some(bytes))
    }

    fn create(language: &TreeSitterLanguage, bytes: Option<&[u8]>) -> Result<Self, Error> {
        let owned_language = language.clone();
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
        Ok(Self {
            language: owned_language,
            raw,
            view,
        })
    }

    pub(crate) fn tables(&self) -> &GrammarView {
        unsafe { self.view.as_ref() }
    }

    /// Clones the underlying tree-sitter language handle.
    ///
    /// **Not in Tree-sitter**
    pub fn tree_sitter_language(&self) -> TreeSitterLanguage {
        self.language.clone()
    }

    /// Check whether this language can be assigned to a parser.
    ///
    /// When Tree-sitter is compiled to WebAssembly, languages obtained from a
    /// syntax tree can be used for parsing only within the same WebAssembly
    /// instance that created the tree. In other instances, such languages can
    /// still be used to inspect syntax trees.
    ///
    /// This reports the underlying language’s capability, independently of direct-parser
    /// restrictions.
    pub fn is_parseable(&self) -> bool {
        self.language.is_parseable()
    }

    /// Get the name of this language. This returns `None` in older parsers.
    pub fn name(&self) -> Option<&str> {
        self.language.name()
    }

    /// Get the ABI version number that indicates which version of the
    /// Tree-sitter CLI that was used to generate this [`Language`].
    pub fn abi_version(&self) -> usize {
        self.language.abi_version()
    }

    /// Get the metadata for this language. This information is generated by the
    /// CLI, and relies on the language author providing the correct metadata in
    /// the language's `tree-sitter.json` file.
    ///
    /// See also [`tree_sitter::LanguageMetadata`].
    pub fn metadata(&self) -> Option<tree_sitter::LanguageMetadata> {
        self.language.metadata()
    }

    /// Get the number of distinct node types in this language.
    pub fn node_kind_count(&self) -> usize {
        self.language.node_kind_count()
    }

    /// Get the number of valid states in this language.
    pub fn parse_state_count(&self) -> usize {
        self.language.parse_state_count()
    }

    /// Get the number of distinct field names in this language.
    pub fn field_count(&self) -> usize {
        self.language.field_count()
    }

    /// Get the name of the node kind for the given numerical id.
    pub fn node_kind_for_id(&self, id: KindId) -> Option<&str> {
        self.language.node_kind_for_id(id.raw())
    }

    /// Check if the node type for the given numerical id is named (as opposed
    /// to an anonymous node type).
    pub fn node_kind_is_named(&self, id: KindId) -> bool {
        self.language.node_kind_is_named(id.raw())
    }

    /// Check if the node type for the given numerical id is visible (as opposed
    /// to a hidden node type).
    pub fn node_kind_is_visible(&self, id: KindId) -> bool {
        self.language.node_kind_is_visible(id.raw())
    }

    /// Check if the node type for the given numerical id is a supertype.
    pub fn node_kind_is_supertype(&self, id: KindId) -> bool {
        self.language.node_kind_is_supertype(id.raw())
    }

    /// Get the field name for the given numerical id.
    pub fn field_name_for_id(&self, id: FieldId) -> Option<&str> {
        self.language.field_name_for_id(id.raw())
    }

    /// Get the numeric id for the given node kind.
    ///
    /// An unsuccessful lookup returns `KindId::from_raw(0)`.
    pub fn id_for_node_kind(&self, kind: &str, named: bool) -> KindId {
        KindId::from_raw(self.language.id_for_node_kind(kind, named))
    }

    /// Get a list of all supertype symbols for the language.
    ///
    /// Borrows the original grammar symbols without allocating or remapping hidden symbols.
    pub fn supertypes(&self) -> &[GrammarId] {
        GrammarId::from_slice(self.language.supertypes())
    }

    /// Get a list of all subtype symbols for a given supertype symbol.
    ///
    /// Borrows the original grammar symbols without allocating or remapping hidden symbols.
    pub fn subtypes_for_supertype(&self, supertype: GrammarId) -> &[GrammarId] {
        GrammarId::from_slice(self.language.subtypes_for_supertype(supertype.raw()))
    }

    /// Resolve a displayed kind name in this grammar.
    ///
    /// **Not in Tree-sitter**. Checks both the name and namedness; returns `None` on
    /// failure instead of the zero sentinel.
    pub fn kind_id_for_name(&self, name: &str, named: bool) -> Option<KindId> {
        let language = self.tree_sitter_language();
        let id = language.id_for_node_kind(name, named);
        (language.node_kind_for_id(id) == Some(name) && self.tables().named(id) == named)
            .then_some(KindId::from_raw(id))
    }

    /// Resolve an original grammar kind, including kinds hidden by aliases.
    ///
    /// **Not in Tree-sitter**. Looks up original grammar symbols, including hidden symbols
    /// and kinds hidden by aliases.
    pub fn grammar_id_for_name(&self, name: &str, named: bool) -> Option<GrammarId> {
        let tables = self.tables();
        (0..tables.grammar_symbol_count as u16)
            .chain([u16::MAX, u16::MAX - 1])
            .find_map(|id| {
                (tables.symbol_name(id) == name && tables.named(id) == named)
                    .then_some(GrammarId::from_raw(id))
            })
    }

    /// A Squatter kind ID for comparing nodes or filtering scans in this language.
    ///
    /// Prepare Tree-sitter IDs once to use the cheaper [`crate::Node::squatter_kind_id`]
    /// accessor and [`crate::Scan::filter_squatter_kind_ids`].
    /// Hidden and noncanonical IDs return `None`.
    pub fn squatter_kind_id(&self, id: KindId) -> Option<SquatterKindId> {
        self.tables().remap_kind(id)
    }

    /// A Squatter grammar ID for comparing original symbols, ignoring aliases.
    ///
    /// Prepare Tree-sitter IDs once to use the cheaper [`crate::Node::squatter_grammar_id`]
    /// accessor with nodes of this language.
    /// Symbols excluded from packed storage return `None`.
    pub fn squatter_grammar_id(&self, id: GrammarId) -> Option<SquatterGrammarId> {
        self.tables().remap_grammar(id)
    }

    /// A displayed kind ID compatible with Tree-sitter's nodes and language APIs.
    ///
    /// Use this when sharing a Squatter kind ID with Tree-sitter.
    /// Zero and out-of-range IDs return `None`.
    pub fn kind_id(&self, id: SquatterKindId) -> Option<KindId> {
        (id.raw() != 0 && u32::from(id.raw()) < self.tables().kind_count + 2)
            .then(|| self.tables().decode_kind(id))
    }

    /// An original grammar ID compatible with Tree-sitter, ignoring aliases.
    ///
    /// Use this when sharing a Squatter grammar ID with Tree-sitter.
    /// Zero and out-of-range IDs return `None`.
    pub fn grammar_id(&self, id: SquatterGrammarId) -> Option<GrammarId> {
        (id.raw() != 0 && u32::from(id.raw()) < self.tables().compact_grammar_count + 2)
            .then(|| self.tables().decode_grammar_kind(id))
    }

    /// Number of compact display ID slots, including reserved zero and both errors.
    pub fn squatter_kind_count(&self) -> usize {
        self.tables().kind_count as usize + 2
    }

    /// Number of compact grammar ID slots, including reserved zero and both errors.
    pub fn squatter_grammar_count(&self) -> usize {
        self.tables().compact_grammar_count as usize + 2
    }

    /// Resolve a displayed name directly into this language's compact domain.
    pub fn squatter_kind_id_for_name(&self, name: &str, named: bool) -> Option<SquatterKindId> {
        self.squatter_kind_id(self.kind_id_for_name(name, named)?)
    }

    /// Resolve an original grammar name directly into this language's compact domain.
    pub fn squatter_grammar_id_for_name(
        &self,
        name: &str,
        named: bool,
    ) -> Option<SquatterGrammarId> {
        self.squatter_grammar_id(self.grammar_id_for_name(name, named)?)
    }

    /// Get the numerical id for the given field name.
    pub fn field_id_for_name(&self, name: impl AsRef<[u8]>) -> Option<FieldId> {
        self.language.field_id_for_name(name).map(FieldId::from)
    }

    /// Serializes prepared grammar metadata for separate
    /// persistence.
    ///
    /// **Not in Tree-sitter**
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
    // stored kind ID; zero denotes a wildcard
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
    pub language: Language,
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
            row: 0,
            column: 0,
            offset: 0,
            kind: tree_sitter::QueryErrorKind::Syntax,
            message: "query exceeds u32 size".into(),
        })?;
        let tables = language.tables();
        let mut offset = 0;
        let mut kind = 0;
        let raw = unsafe {
            sq_native_query_new(
                tables.language.cast(),
                source.as_ptr(),
                length,
                &mut offset,
                &mut kind,
            )
        };
        let raw = NonNull::new(raw).ok_or_else(|| {
            if kind == 6 {
                QueryError {
                    row: 0,
                    column: 0,
                    offset: 0,
                    message: tree_sitter::LanguageError::Version(
                        language.tree_sitter_language().abi_version(),
                    )
                    .to_string(),
                    kind: tree_sitter::QueryErrorKind::Language,
                }
            } else {
                QueryError::compile(source, offset as usize, kind)
            }
        })?;
        let mut view = MaybeUninit::uninit();
        unsafe {
            sq_native_query_view(raw.as_ptr(), view.as_mut_ptr());
        }
        let mut result = Self {
            raw,
            view: unsafe { view.assume_init() },
            language: language.clone(),
        };
        result.view.symbol_count = tables.kind_count;
        // Native mutations only remove entries or captures; symbols stay encoded.
        for step in result.steps_mut() {
            if step.symbol != 0 {
                step.symbol = tables
                    .remap_kind(KindId::from_raw(step.symbol))
                    .ok_or_else(|| QueryError {
                        row: 0,
                        column: 0,
                        offset: 0,
                        kind: tree_sitter::QueryErrorKind::Structure,
                        message: "query kind cannot occur in packed storage".into(),
                    })?
                    .raw();
            }
        }
        #[cfg(debug_assertions)]
        result.validate();
        Ok(result)
    }

    pub fn deep_clone(&self) -> Self {
        let raw = NonNull::new(unsafe { sq_native_query_copy(self.raw.as_ptr()) }).unwrap();
        let mut view = MaybeUninit::uninit();
        unsafe {
            sq_native_query_view(raw.as_ptr(), view.as_mut_ptr());
        }
        let mut view = unsafe { view.assume_init() };
        view.symbol_count = self.view.symbol_count;
        Self {
            raw,
            view,
            language: self.language.clone(),
        }
    }

    pub fn is_pattern_guaranteed_at_step(&self, offset: usize) -> bool {
        unsafe { sq_native_query_is_pattern_guaranteed_at_step(self.raw.as_ptr(), offset as u32) }
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
        }
        self.refresh();
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

    fn refresh(&mut self) {
        unsafe {
            sq_native_query_view(self.raw.as_ptr(), &mut self.view);
        }
        self.view.symbol_count = self.language.tables().kind_count;
        #[cfg(debug_assertions)]
        self.validate();
    }

    #[cfg(debug_assertions)]
    fn validate(&self) {
        let steps = self.steps();
        for step in steps {
            assert!((step.symbol as u32) < self.view.symbol_count + 2);
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
            assert!((entry.pattern_index.raw() as usize) < self.patterns().len());
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
    fn sq_native_query_copy(query: *const QueryHandle) -> *mut QueryHandle;
    fn sq_native_query_is_pattern_guaranteed_at_step(
        query: *const QueryHandle,
        offset: u32,
    ) -> bool;
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
struct ParserInput {
    payload: *mut c_void,
    read: unsafe extern "C" fn(*mut c_void, u32, Point, *mut u32) -> *const u8,
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

    pub fn language(&self) -> &Language {
        &self.language
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

    pub fn parse_chunks<T: AsRef<[u8]>, F: FnMut(usize, tree_sitter::Point) -> T>(
        &mut self,
        callback: &mut F,
    ) -> Result<Reductions<'_>, crate::ParseError> {
        struct Payload<'a, F, T> {
            callback: &'a mut F,
            text: Option<T>,
            panic: Option<Box<dyn std::any::Any + Send>>,
            overflow: Option<(u32, Point)>,
        }

        unsafe extern "C" fn read<T: AsRef<[u8]>, F: FnMut(usize, tree_sitter::Point) -> T>(
            payload: *mut c_void,
            byte: u32,
            point: Point,
            size: *mut u32,
        ) -> *const u8 {
            let payload = unsafe { &mut *payload.cast::<Payload<F, T>>() };
            unsafe { *size = 0 };
            if payload.panic.is_some() || payload.overflow.is_some() {
                return std::ptr::null();
            }
            // Keep owned chunks alive until the next read. Unwind only after C
            // has released its parse state and stopped borrowing the callback.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                payload.text = Some((payload.callback)(
                    byte as usize,
                    tree_sitter::Point::new(point.row as usize, point.column as usize),
                ));
                let source = payload.text.as_ref().unwrap().as_ref();
                if source.len() > (u32::MAX - byte) as usize {
                    payload.overflow = Some((byte, point));
                    return std::ptr::null();
                }
                unsafe { *size = source.len() as u32 };
                source.as_ptr()
            }));
            match result {
                Ok(source) => source,
                Err(panic) => {
                    payload.panic = Some(panic);
                    std::ptr::null()
                }
            }
        }

        let mut payload = Payload {
            callback,
            text: None::<T>,
            panic: None,
            overflow: None,
        };
        let mut status = ParseStatus::new();
        let success = unsafe {
            sq_native_parser_parse_with_callback(
                self.raw.as_ptr(),
                ParserInput {
                    payload: (&mut payload as *mut Payload<F, T>).cast(),
                    read: read::<T, F>,
                },
                &mut status,
            )
        };
        if payload.panic.is_some() || payload.overflow.is_some() {
            unsafe { sq_native_parser_clear(self.raw.as_ptr()) };
        }
        if let Some(panic) = payload.panic {
            std::panic::resume_unwind(panic);
        }
        if let Some((byte, point)) = payload.overflow {
            return Err(crate::ParseError {
                code: Error::Overflow,
                byte,
                point: tree_sitter::Point::new(point.row as usize, point.column as usize),
                message: "source exceeds the 32-bit byte limit".into(),
            });
        }
        if !success {
            return Err(status.into_error());
        }
        Ok(Reductions(self))
    }

    pub fn drop_scratch(&mut self) {
        unsafe {
            sq_native_parser_drop_scratch(self.raw.as_ptr());
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
    fn sq_native_parser_drop_scratch(parser: *mut ParserHandle);
    fn sq_native_parser_clear(parser: *mut ParserHandle);
    fn sq_native_parser_parse(
        parser: *mut ParserHandle,
        source: *const u8,
        length: u32,
        error: *mut ParseStatus,
    ) -> bool;
    fn sq_native_parser_parse_with_callback(
        parser: *mut ParserHandle,
        input: ParserInput,
        error: *mut ParseStatus,
    ) -> bool;
    fn sq_native_parser_reductions(
        parser: *const ParserHandle,
        count: *mut u32,
        root: *mut u32,
    ) -> *const Reduction;
}
