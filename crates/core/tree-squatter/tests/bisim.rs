//! Pool operations are repaired again after shrinking; printed operands are runtime indices.
mod support;

use proptest::{
    prelude::*,
    test_runner::{Config, FileFailurePersistence, RngSeed, TestCaseError, TestRunner},
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap},
    ops::{ControlFlow, Range},
    panic::{AssertUnwindSafe, catch_unwind},
    slice,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicUsize, Ordering},
    },
};
use tree_sitter::{Point, StreamingIterator};
use tree_squatter::{
    CaptureIx, ChildIx, Error, FieldId, Forest, GrammarId, Language, NamedChildIx, Node, NodeId,
    PackOptions, PackRegion, PackedParseOptions, Packer, PatternIx, PointsData, PresenceCache,
    Query, QueryCursor, QueryCursorOptions, QueryCursorState, QueryExecutionError, QueryScope,
    StableSlab, TreeFellerParser, TreeIx,
    scan::{GroupScan, Scan},
    traits::{NodeLike, Parse, ParseStateLike},
};

const MAX_BYTES: usize = 8192;
const MAX_FORESTS: usize = 8;
const MAX_STEPS: usize = 256;
const MAX_RESULTS: usize = 16384;
const INSERTIONS: &[&str] = &[
    "",
    " ",
    "\n",
    "\r\n",
    ",",
    "0",
    "\"π😀\"",
    "[]",
    "{}",
    "null",
    "(",
    "}",
    "abc",
];
const QUERIES: &[&str] = &[
    "(_) @any",
    "(_ (_) @child) @parent",
    "(ERROR) @error",
    "(_ . (_) @first)",
    "((_) @left . (_) @right)",
    "((_) @value (#eq? @value \"1\"))",
    "((_) @value (#match? @value \"^[a-z]\"))",
    "(_ (_) * @children) @parent",
    "(_) @first @second\n(_) @third",
];

struct Fixture {
    name: &'static str,
    native: tree_sitter::Language,
    packed: Language,
    sources: &'static [&'static str],
    invalid: &'static str,
}

static LANGUAGES: LazyLock<Vec<Fixture>> = LazyLock::new(|| {
    let python = unsafe {
        tree_sitter::Language::from_raw(tree_sitter_python::LANGUAGE.into_raw()().cast())
    };
    let rust =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_rust::LANGUAGE.into_raw()().cast()) };
    [
        (
            "JSON",
            support::json_language(),
            &[
                "{\"a\": [1, true, null]}",
                "",
                "[1,",
                "[\r\n\"π😀\", 2]",
                "{\"x\": [[], {}]}",
            ] as &[_],
            "{",
        ),
        (
            "C",
            support::c_language(),
            &[
                "int value;",
                "",
                "int f(int x) { /* extra */ return x + 1; }",
                "int broken = ;",
                "int f() { return 1 }",
            ],
            "int broken = ;",
        ),
        (
            "C#",
            support::c_sharp_language(),
            &[
                "class C { int F(int x) => x + 1; }",
                "",
                "class C { int x = ; }",
                "// π😀\r\nclass C {}",
            ],
            "class {",
        ),
        (
            "Python",
            python,
            &[
                "if True:\n    if False:\n        pass\n    else:\n        pass\nx = 1\n",
                "",
                "def f(value):\n\treturn f'hello {value!r:>10} π😀'\n",
                "text = f'unterminated {",
            ],
            "text = f'unterminated {",
        ),
        (
            "Rust",
            rust,
            &[
                "/* leading */ fn main() {} /* trailing */",
                "",
                "fn main() { let value = /* outer /* inner */ end */ 1; }",
                "/* unterminated",
            ],
            "/* unterminated",
        ),
    ]
    .into_iter()
    .map(|(name, native, sources, invalid)| Fixture {
        packed: Language::new(&native).unwrap(),
        name,
        native,
        sources,
        invalid,
    })
    .collect()
});

#[derive(Clone, Debug, PartialEq, Eq)]
struct Document {
    language: usize,
    source: Arc<str>,
    valid: bool,
}

fn fixture(language: usize, source: usize) -> Document {
    Document {
        language,
        source: LANGUAGES[language].sources[source].into(),
        valid: match language {
            0 => source != 1 && source != 2,
            1 => source < 3,
            2 => source != 2,
            _ => source < 3,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Presence {
    Default,
    None,
    All,
    Selected(u8),
}

impl Presence {
    fn selects(self, index: usize) -> bool {
        match self {
            Self::All => true,
            Self::Selected(mask) => mask & (1 << (index % 8)) != 0,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Options {
    points: bool,
    compact: bool,
    presence: Presence,
}

impl Options {
    fn pack<'a>(
        &self,
        selection: &'a dyn Fn(tree_squatter::ForestRegion<'_>) -> bool,
    ) -> PackOptions<'a> {
        PackOptions {
            points: self.points,
            compact: self.compact,
            symbol_presence: if self.presence == Presence::Default {
                PackOptions::default().symbol_presence
            } else {
                selection
            },
            ..Default::default()
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            points: true,
            compact: false,
            presence: Presence::Default,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Position {
    Zero,
    Start,
    End,
    AfterEnd,
    Raw(u16),
}

impl Position {
    fn resolve(self, node: tree_sitter::Node<'_>) -> usize {
        match self {
            Self::Zero => 0,
            Self::Start => node.start_byte(),
            Self::End => node.end_byte(),
            Self::AfterEnd => node.end_byte() + 1,
            Self::Raw(value) => value as usize,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Child {
    First,
    Last,
    PastLast,
    Large,
    Raw(u8),
}

impl Child {
    fn resolve(self, count: usize) -> usize {
        match self {
            Self::First => 0,
            Self::Last => count.saturating_sub(1),
            Self::PastLast => count,
            Self::Large => u32::MAX as usize,
            Self::Raw(value) => value as usize,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopyMode {
    Compact,
    Detach,
    Load,
    SafetyChecked,
    Retain,
    TrustedLoad,
    TrustedRetain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Points,
    Presence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RootSpecification {
    document: usize,
    subtree: u8,
    identity: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ForestMetadata {
    identity: usize,
    layout: usize,
    regions: Vec<Vec<RootSpecification>>,
    options: Options,
    points: bool,
    presence: Option<Presence>,
    retained: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct QueryMetadata {
    language: usize,
    source: usize,
    patterns: BTreeSet<usize>,
    captures: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CursorConfig {
    byte_range: Option<Range<usize>>,
    containing: Option<Range<usize>>,
    depth: Option<u32>,
    limit: Option<u32>,
    point: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SavedMetadata {
    layout: usize,
    side: Side,
    presence: Option<Presence>,
}

#[derive(Clone, Debug)]
enum Operation {
    Add {
        language: usize,
        source: usize,
    },
    Splice {
        document: usize,
        start: u16,
        end: u16,
        insertion: usize,
    },
    NewParser {
        language: usize,
    },
    ParserScratch(usize),
    ResetParser(usize),
    DropParser(usize),
    NewPacker,
    PackerScratch(usize),
    DropPacker(usize),
    Parse {
        parser: usize,
        document: usize,
        options: Options,
        chunk: u8,
    },
    Pack {
        packer: usize,
        regions: Vec<Vec<(usize, u8)>>,
        options: Options,
        mixed: bool,
    },
    InvalidParse(usize),
    InvalidRegion(usize),
    CancelParse(usize),
    CancelPresence(usize),
    Copy {
        forest: usize,
        mode: CopyMode,
    },
    Compact(usize),
    DropForest(usize),
    Save {
        forest: usize,
        side: Side,
    },
    Restore {
        forest: usize,
        saved: usize,
        retained: bool,
    },
    DropSaved(usize),
    DropSide {
        forest: usize,
        side: Side,
    },
    BuildPresence {
        forest: usize,
        presence: Presence,
    },
    InvalidSide(usize),
    NewQuery {
        language: usize,
        source: usize,
    },
    CloneQuery(usize),
    DropQuery(usize),
    DisablePattern {
        query: usize,
        pattern: usize,
    },
    DisableCapture {
        query: usize,
        capture: usize,
    },
    InvalidQuery(usize),
    NewQueryCursor,
    DropQueryCursor(usize),
    Configure {
        cursor: usize,
        config: CursorConfig,
    },
    Execute {
        forest: usize,
        region: usize,
        query: usize,
        cursor: usize,
        script: u8,
        scope: u8,
    },
    WrongGrammar {
        forest: usize,
        query: usize,
        cursor: usize,
    },
    Explore(Vec<ViewOperation>),
}

#[derive(Clone, Debug)]
enum Navigation {
    Parent,
    Child(Child),
    NamedChild(Child),
    Next,
    Previous,
    NextNamed,
    PreviousNamed,
    Field(usize),
    FirstByte(Position),
    FirstNamedByte(Position),
    Descendant(Position, Position, bool, bool),
}

#[derive(Clone, Debug)]
enum ViewOperation {
    Read(usize),
    Navigate {
        node: usize,
        navigation: Navigation,
    },
    DropNode(usize),
    NewCursor(usize),
    CloneCursor(usize),
    DropCursor(usize),
    ReadCursor(usize),
    Move {
        cursor: usize,
        movement: u8,
        position: Position,
    },
    Reset {
        cursor: usize,
        node: usize,
    },
    ResetTo {
        cursor: usize,
        other: usize,
    },
    Scan {
        node: usize,
        recipe: ScanRecipe,
    },
}

#[derive(Clone, Copy, Debug)]
struct ScanRecipe {
    postorder: bool,
    reverse: bool,
    relation: u8,
    start: Position,
    end: Position,
    filter: u8,
    consume: u8,
    points: bool,
}

#[derive(Clone, Debug)]
struct Case {
    documents: Vec<Document>,
    operations: Vec<Operation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Model {
    documents: Vec<Document>,
    parsers: Vec<usize>,
    packers: usize,
    forests: Vec<ForestMetadata>,
    saved: Vec<SavedMetadata>,
    queries: Vec<QueryMetadata>,
    cursors: Vec<CursorConfig>,
    next_identity: usize,
}

impl Model {
    fn new(documents: Vec<Document>) -> Self {
        Self {
            documents,
            parsers: vec![0],
            packers: 1,
            forests: vec![ForestMetadata {
                identity: 0,
                layout: 0,
                regions: vec![vec![RootSpecification {
                    document: 0,
                    subtree: 0,
                    identity: 1,
                }]],
                options: Options::default(),
                points: true,
                presence: Some(Presence::Default),
                retained: false,
            }],
            saved: Vec::new(),
            queries: Vec::new(),
            cursors: Vec::new(),
            next_identity: 2,
        }
    }
    fn within_budget(&self) -> bool {
        let roots: Vec<_> = self
            .forests
            .iter()
            .flat_map(|forest| forest.regions.iter().flatten())
            .collect();
        self.documents.len() <= 64
            && self
                .documents
                .iter()
                .all(|document| document.source.len() <= MAX_BYTES)
            && self.forests.len() <= MAX_FORESTS
            && roots.len() <= 24
            && roots
                .iter()
                .map(|root| self.documents[root.document].source.len())
                .sum::<usize>()
                <= MAX_BYTES * 2
            && self.parsers.len() <= 8
            && self.packers <= 8
            && self.saved.len() <= 8
            && self.queries.len() <= 8
            && self.cursors.len() <= 8
    }

    fn identity(&mut self) -> usize {
        let identity = self.next_identity;
        self.next_identity += 1;
        identity
    }
    fn forest(&mut self, regions: &[Vec<(usize, u8)>], options: Options) -> ForestMetadata {
        let identity = self.identity();
        ForestMetadata {
            identity,
            layout: identity,
            regions: regions
                .iter()
                .map(|region| {
                    region
                        .iter()
                        .map(|&(document, subtree)| RootSpecification {
                            document,
                            subtree,
                            identity: self.identity(),
                        })
                        .collect()
                })
                .collect(),
            options,
            points: options.points,
            presence: if options.presence == Presence::Default
                || (0..regions.len()).any(|index| options.presence.selects(index))
            {
                Some(options.presence)
            } else {
                None
            },
            retained: false,
        }
    }
    fn apply(&mut self, operation: &Operation) {
        match operation {
            Operation::Add { language, source } => self.documents.push(fixture(*language, *source)),
            Operation::Splice {
                document,
                start,
                end,
                insertion,
            } => {
                let original = &self.documents[*document];
                let boundary = |offset: u16| {
                    let mut offset = (offset as usize).min(original.source.len());
                    while !original.source.is_char_boundary(offset) {
                        offset -= 1;
                    }
                    offset
                };
                let (start, end) = (boundary(*start), boundary(*end));
                let mut source = original.source.to_string();
                source.replace_range(start.min(end)..start.max(end), INSERTIONS[*insertion]);
                assert!(source.len() <= MAX_BYTES);
                self.documents.push(Document {
                    language: original.language,
                    source: source.into(),
                    valid: false,
                });
            }
            Operation::NewParser { language } => self.parsers.push(*language),
            Operation::DropParser(index) => {
                self.parsers.remove(*index);
            }
            Operation::NewPacker => self.packers += 1,
            Operation::DropPacker(_) => self.packers -= 1,
            Operation::Parse {
                document, options, ..
            } => {
                let metadata = self.forest(&[vec![(*document, 0)]], *options);
                self.forests.push(metadata);
            }
            Operation::Pack {
                regions, options, ..
            } => {
                let metadata = self.forest(regions, *options);
                self.forests.push(metadata);
            }
            Operation::Copy { forest, mode } => {
                let mut metadata = self.forests[*forest].clone();
                metadata.identity = self.identity();
                metadata.retained = matches!(mode, CopyMode::Retain | CopyMode::TrustedRetain);
                if !matches!(mode, CopyMode::Compact | CopyMode::Detach) {
                    metadata.points = false;
                    metadata.presence = None;
                }
                self.forests.push(metadata);
            }
            Operation::Compact(index) => self.forests[*index].retained = false,
            Operation::DropForest(index) => {
                self.forests.remove(*index);
            }
            Operation::Save { forest, side } => self.saved.push(SavedMetadata {
                layout: self.forests[*forest].layout,
                side: *side,
                presence: self.forests[*forest].presence,
            }),
            Operation::Restore { forest, saved, .. } => match self.saved[*saved].side {
                Side::Points => self.forests[*forest].points = true,
                Side::Presence => self.forests[*forest].presence = self.saved[*saved].presence,
            },
            Operation::DropSaved(index) => {
                self.saved.remove(*index);
            }
            Operation::DropSide { forest, side } => match side {
                Side::Points => self.forests[*forest].points = false,
                Side::Presence => self.forests[*forest].presence = None,
            },
            Operation::BuildPresence { forest, presence } => {
                self.forests[*forest].presence = Some(*presence)
            }
            Operation::NewQuery { language, source } => self.queries.push(QueryMetadata {
                language: *language,
                source: *source,
                patterns: BTreeSet::new(),
                captures: BTreeSet::new(),
            }),
            Operation::CloneQuery(index) => self.queries.push(self.queries[*index].clone()),
            Operation::DropQuery(index) => {
                self.queries.remove(*index);
            }
            Operation::DisablePattern { query, pattern } => {
                self.queries[*query].patterns.insert(*pattern);
            }
            Operation::DisableCapture { query, capture } => {
                let name = capture_names(self.queries[*query].source)[*capture];
                self.queries[*query].captures.insert(name.into());
            }
            Operation::NewQueryCursor => self.cursors.push(CursorConfig::default()),
            Operation::DropQueryCursor(index) => {
                self.cursors.remove(*index);
            }
            Operation::Configure { cursor, config } => {
                let previous = &self.cursors[*cursor];
                let valid = |range: &Option<Range<usize>>| {
                    range
                        .as_ref()
                        .is_none_or(|range| range.end == 0 || range.start <= range.end)
                };
                let mut effective = config.clone();
                if !valid(&config.byte_range) {
                    effective.byte_range = previous.byte_range.clone();
                }
                if !valid(&config.containing) {
                    effective.containing = previous.containing.clone();
                }
                if !valid(&config.byte_range) && !valid(&config.containing) {
                    effective.point = previous.point;
                }
                self.cursors[*cursor] = effective;
            }
            Operation::Execute { cursor, script, .. } => {
                if matches!(script % 8, 3 | 6) {
                    self.cursors[*cursor].byte_range = None;
                }
            }
            Operation::ParserScratch(_)
            | Operation::ResetParser(_)
            | Operation::PackerScratch(_)
            | Operation::InvalidParse(_)
            | Operation::InvalidRegion(_)
            | Operation::CancelParse(_)
            | Operation::CancelPresence(_)
            | Operation::InvalidSide(_)
            | Operation::InvalidQuery(_)
            | Operation::WrongGrammar { .. }
            | Operation::Explore(_) => {}
        }
    }
}

fn capture_names(source: usize) -> Vec<&'static str> {
    match source {
        0 => vec!["any"],
        1 => vec!["child", "parent"],
        2 => vec!["error"],
        3 => vec!["first"],
        4 => vec!["left", "right"],
        5 | 6 => vec!["value"],
        7 => vec!["children", "parent"],
        8 => vec!["first", "second", "third"],
        _ => unreachable!(),
    }
}

fn select(index: &mut usize, count: usize) -> bool {
    if count == 0 {
        false
    } else {
        *index %= count;
        true
    }
}

fn compatible(index: &mut usize, choices: impl Iterator<Item = usize>) -> bool {
    let choices: Vec<_> = choices.collect();
    if choices.is_empty() {
        false
    } else {
        *index = choices[*index % choices.len()];
        true
    }
}

#[derive(Default, Debug)]
struct Coverage {
    generated: usize,
    admitted: usize,
    executed: usize,
    absent: usize,
    budget: usize,
    views: usize,
    navigation: [usize; 2],
    // native, parse-and-pack, explicit pack, direct attempts
    parses: [usize; 4],
    direct_failures: usize,
    copies: [usize; 7],
    mixed: usize,
    nodes: usize,
    slab_bytes: usize,
    scans: usize,
    matches: usize,
    failures: usize,
}

fn repair(case: &Case, coverage: &mut Coverage) -> Case {
    let mut model = Model::new(case.documents.clone());
    let mut operations = Vec::new();
    let mut steps = 0;
    for mut operation in case.operations.clone() {
        coverage.generated += 1;
        let admitted = match &mut operation {
            Operation::Add { language, source } => {
                *language %= LANGUAGES.len();
                *source %= LANGUAGES[*language].sources.len();
                true
            }
            Operation::Splice {
                document,
                insertion,
                ..
            } => {
                *insertion %= INSERTIONS.len();
                select(document, model.documents.len())
            }
            Operation::NewParser { language } => {
                *language %= LANGUAGES.len();
                true
            }
            Operation::ParserScratch(index) | Operation::ResetParser(index) => {
                select(index, model.parsers.len())
            }
            Operation::DropParser(index) => select(index, model.parsers.len()),
            Operation::NewPacker | Operation::NewQueryCursor => true,
            Operation::PackerScratch(index)
            | Operation::DropPacker(index)
            | Operation::InvalidRegion(index) => select(index, model.packers),
            Operation::Parse {
                parser, document, ..
            } => {
                select(parser, model.parsers.len())
                    && compatible(
                        document,
                        model
                            .documents
                            .iter()
                            .enumerate()
                            .filter(|(_, document)| document.language == model.parsers[*parser])
                            .map(|(index, _)| index),
                    )
            }
            Operation::Pack {
                packer,
                regions,
                mixed,
                ..
            } => {
                if !select(packer, model.packers) {
                    false
                } else {
                    for region in regions.iter_mut() {
                        if let Some((document, _)) = region.first_mut() {
                            select(document, model.documents.len());
                            let language = model.documents[*document].language;
                            for (document, _) in region.iter_mut().skip(1) {
                                compatible(
                                    document,
                                    model
                                        .documents
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, document)| document.language == language)
                                        .map(|(index, _)| index),
                                );
                            }
                        }
                    }
                    regions.iter().all(|region| !region.is_empty())
                        && (!*mixed
                            || regions
                                .iter()
                                .map(|region| model.documents[region[0].0].language)
                                .collect::<BTreeSet<_>>()
                                .len()
                                >= 2)
                }
            }
            Operation::InvalidParse(index) => compatible(
                index,
                model
                    .parsers
                    .iter()
                    .enumerate()
                    .filter(|(_, language)| LANGUAGES[**language].native.abi_version() >= 15)
                    .map(|(index, _)| index),
            ),
            Operation::CancelParse(index) => select(index, model.parsers.len()),
            Operation::Copy { forest, .. }
            | Operation::Compact(forest)
            | Operation::DropForest(forest)
            | Operation::CancelPresence(forest)
            | Operation::InvalidSide(forest)
            | Operation::BuildPresence { forest, .. }
            | Operation::DropSide { forest, .. } => select(forest, model.forests.len()),
            Operation::Save { forest, side } => compatible(
                forest,
                model
                    .forests
                    .iter()
                    .enumerate()
                    .filter(|(_, forest)| match side {
                        Side::Points => forest.points,
                        Side::Presence => forest
                            .presence
                            .is_some_and(|presence| presence != Presence::Default),
                    })
                    .map(|(index, _)| index),
            ),
            Operation::Restore { forest, saved, .. } => {
                select(saved, model.saved.len())
                    && compatible(
                        forest,
                        model
                            .forests
                            .iter()
                            .enumerate()
                            .filter(|(_, forest)| forest.layout == model.saved[*saved].layout)
                            .map(|(index, _)| index),
                    )
            }
            Operation::DropSaved(index) => select(index, model.saved.len()),
            Operation::NewQuery { language, source } => {
                *language %= LANGUAGES.len();
                *source %= QUERIES.len();
                true
            }
            Operation::CloneQuery(index) | Operation::DropQuery(index) => {
                select(index, model.queries.len())
            }
            Operation::DisablePattern { query, pattern } => {
                *pattern %=
                    if select(query, model.queries.len()) && model.queries[*query].source == 8 {
                        2
                    } else {
                        1
                    };
                !model.queries.is_empty()
            }
            Operation::DisableCapture { query, capture } => {
                select(query, model.queries.len())
                    && select(capture, capture_names(model.queries[*query].source).len())
            }
            Operation::InvalidQuery(language) => {
                *language %= LANGUAGES.len();
                true
            }
            Operation::DropQueryCursor(index) | Operation::Configure { cursor: index, .. } => {
                select(index, model.cursors.len())
            }
            Operation::Execute {
                forest,
                region,
                query,
                cursor,
                scope,
                ..
            } => {
                let valid = select(query, model.queries.len())
                    && select(cursor, model.cursors.len())
                    && compatible(
                        forest,
                        model
                            .forests
                            .iter()
                            .enumerate()
                            .filter(|(_, forest)| {
                                forest.regions.iter().any(|region| {
                                    model.documents[region[0].document].language
                                        == model.queries[*query].language
                                })
                            })
                            .map(|(index, _)| index),
                    )
                    && compatible(
                        region,
                        model.forests[*forest]
                            .regions
                            .iter()
                            .enumerate()
                            .filter(|(_, region)| {
                                model.documents[region[0].document].language
                                    == model.queries[*query].language
                            })
                            .map(|(index, _)| index),
                    );
                if valid {
                    let config = &model.cursors[*cursor];
                    let roots = &model.forests[*forest].regions[*region];
                    let bounded = config.byte_range.is_some() || config.containing.is_some();
                    // Tree-sitter drops deferred matches on malformed trees with both ranges.
                    // See containing_ranges_finish_deferred_matches_in_error_subtrees.
                    let comparable = config.byte_range.is_none()
                        || config.containing.is_none()
                        || roots
                            .iter()
                            .all(|root| model.documents[root.document].valid);
                    let shared_source = roots.iter().all(|root| root.document == roots[0].document);
                    comparable && (!bounded || *scope % 4 != 2 || shared_source)
                } else {
                    false
                }
            }
            Operation::WrongGrammar {
                forest,
                query,
                cursor,
            } => {
                select(query, model.queries.len())
                    && select(cursor, model.cursors.len())
                    && compatible(
                        forest,
                        model
                            .forests
                            .iter()
                            .enumerate()
                            .filter(|(_, forest)| {
                                !forest.regions.is_empty()
                                    && model.documents[forest.regions[0][0].document].language
                                        != model.queries[*query].language
                            })
                            .map(|(index, _)| index),
                    )
            }
            Operation::Explore(views) => {
                let roots = model
                    .forests
                    .iter()
                    .map(|forest| forest.regions.iter().map(Vec::len).sum::<usize>())
                    .sum::<usize>()
                    * 2;
                *views = repair_views(views, roots);
                roots != 0
            }
        };
        if !admitted {
            coverage.absent += 1;
            continue;
        }
        let cost = 1 + if let Operation::Explore(views) = &operation {
            views.len()
        } else {
            0
        };
        let mut next = model.clone();
        next.apply(&operation);
        if steps + cost > MAX_STEPS || !next.within_budget() {
            coverage.budget += 1;
            continue;
        }
        model = next;
        steps += cost;
        operations.push(operation);
        coverage.admitted += 1;
    }
    Case {
        documents: case.documents.clone(),
        operations,
    }
}

fn repair_views(operations: &[ViewOperation], roots: usize) -> Vec<ViewOperation> {
    let (mut nodes, mut cursors) = (roots, roots);
    operations
        .iter()
        .cloned()
        .filter_map(|mut operation| {
            let admitted = match &mut operation {
                ViewOperation::Read(node) | ViewOperation::Scan { node, .. } => select(node, nodes),
                ViewOperation::Navigate { node, .. } => {
                    let valid = select(node, nodes);
                    if valid {
                        nodes += 1;
                    }
                    valid
                }
                ViewOperation::DropNode(node) => {
                    let valid = select(node, nodes);
                    if valid {
                        nodes -= 1;
                    }
                    valid
                }
                ViewOperation::NewCursor(node) => {
                    let valid = select(node, nodes);
                    if valid {
                        cursors += 1;
                    }
                    valid
                }
                ViewOperation::CloneCursor(cursor) => {
                    let valid = select(cursor, cursors);
                    if valid {
                        cursors += 1;
                    }
                    valid
                }
                ViewOperation::DropCursor(cursor) => {
                    let valid = select(cursor, cursors);
                    if valid {
                        cursors -= 1;
                    }
                    valid
                }
                ViewOperation::ReadCursor(cursor) => {
                    let valid = select(cursor, cursors);
                    if valid {
                        nodes += 1;
                    }
                    valid
                }
                ViewOperation::Move { cursor, .. } => select(cursor, cursors),
                ViewOperation::Reset { cursor, node } => {
                    select(cursor, cursors) && select(node, nodes)
                }
                ViewOperation::ResetTo { cursor, other } => {
                    select(cursor, cursors) && select(other, cursors)
                }
            };
            admitted.then_some(operation)
        })
        .collect()
}

struct ReferenceRoot {
    specification: RootSpecification,
    tree: tree_sitter::Tree,
    path: Vec<usize>,
}

impl Clone for ReferenceRoot {
    fn clone(&self) -> Self {
        Self {
            specification: self.specification.clone(),
            tree: self.tree.clone(),
            path: self.path.clone(),
        }
    }
}

impl ReferenceRoot {
    fn node(&self) -> tree_sitter::Node<'_> {
        self.path
            .iter()
            .fold(self.tree.root_node(), |node, &child| {
                node.child(child as u32).unwrap()
            })
    }
}

struct ForestEntry {
    forest: Forest,
    metadata: ForestMetadata,
    roots: Vec<ReferenceRoot>,
    native_ids: Vec<HashMap<usize, usize>>,
    end_bytes: Vec<Vec<usize>>,
    packed_ids: HashMap<NodeId, (usize, usize)>,
    owners: [Option<Arc<AtomicUsize>>; 3],
}

fn check_attributes(actual: Node<'_>, expected: tree_sitter::Node<'_>, source: &str) {
    let mut attributes = NodeLike::attributes(expected);
    if !actual.has_points() {
        attributes.has_points = false;
        attributes.start_position = Point::new(0, expected.start_byte());
        attributes.end_position = Point::new(0, expected.end_byte());
    }
    assert_eq!(actual.attributes(), attributes);
    assert_eq!(actual.child_count().ix(), expected.child_count() as usize);
    assert_eq!(
        actual.named_child_count().ix(),
        expected.named_child_count()
    );
    assert_eq!(actual.descendant_count(), expected.descendant_count());
    assert_eq!(
        actual.utf8_text(source.as_bytes()),
        expected.utf8_text(source.as_bytes())
    );
    assert_eq!(actual.has_children(), expected.child_count() != 0);
    assert_eq!(
        actual.has_named_children(),
        expected.named_child_count() != 0
    );
}

impl ForestEntry {
    fn new(
        forest: Forest,
        metadata: ForestMetadata,
        roots: Vec<ReferenceRoot>,
        documents: &[Document],
    ) -> Self {
        let mut entry = Self {
            forest,
            metadata,
            roots,
            native_ids: Vec::new(),
            end_bytes: Vec::new(),
            packed_ids: HashMap::new(),
            owners: Default::default(),
        };
        entry.correspondence(documents);
        entry
    }
    fn correspondence(&mut self, documents: &[Document]) {
        self.forest.validate().unwrap();
        assert_eq!(self.forest.trees().len(), self.roots.len());
        assert_eq!(self.forest.regions().len(), self.metadata.regions.len());
        for (region, specs) in self.forest.regions().zip(&self.metadata.regions) {
            assert_eq!(region.trees().len(), specs.len());
            assert_eq!(
                region.language().tree_sitter_language(),
                LANGUAGES[documents[specs[0].document].language].native
            );
        }
        for (tree_index, (tree, reference)) in self.forest.trees().zip(&self.roots).enumerate() {
            let source = &documents[reference.specification.document].source;
            let (mut packed, mut native) = (tree.walk(), reference.node().walk());
            let mut native_ids = HashMap::new();
            let mut end_bytes = Vec::new();
            let mut ordinal = 0;
            loop {
                check_attributes(packed.node(), native.node(), source);
                assert_eq!(packed.depth(), native.depth());
                assert_eq!(
                    packed.field_id().map(FieldId::raw),
                    native.field_id().map(|field| field.get())
                );
                assert_eq!(packed.field_name(), native.field_name());
                assert_eq!(packed.node().id().tree().ix(), tree_index);
                if ordinal == 0 {
                    assert!(packed.node().parent().is_none());
                    assert!(packed.node().next_sibling().is_none());
                    assert!(packed.node().prev_sibling().is_none());
                    assert!(packed.node().field_id().is_none());
                }
                assert!(native_ids.insert(native.node().id(), ordinal).is_none());
                assert!(
                    self.packed_ids
                        .insert(packed.node().id(), (tree_index, ordinal))
                        .is_none()
                );
                end_bytes.push(native.node().end_byte());
                ordinal += 1;
                let descended = native.goto_first_child();
                assert_eq!(packed.goto_first_child(), descended);
                if descended {
                    continue;
                }
                loop {
                    let advanced = native.goto_next_sibling();
                    assert_eq!(packed.goto_next_sibling(), advanced);
                    if advanced {
                        break;
                    }
                    let ascended = native.goto_parent();
                    assert_eq!(packed.goto_parent(), ascended);
                    if !ascended {
                        break;
                    }
                }
                if native.depth() == 0 {
                    break;
                }
            }
            assert_eq!(ordinal, reference.node().descendant_count());
            self.native_ids.push(native_ids);
            self.end_bytes.push(end_bytes);
        }
        self.check(documents);
    }
    fn check(&self, documents: &[Document]) {
        self.forest.validate().unwrap();
        assert_eq!(self.forest.has_points(), self.metadata.points);
        match self.metadata.presence {
            None => assert!(self.forest.presence_cache().is_none()),
            Some(Presence::Default) => {}
            Some(_) => self
                .forest
                .presence_cache()
                .expect("missing modeled presence attachment")
                .validate_for(&self.forest)
                .unwrap(),
        }
        for (tree_index, (tree, reference)) in self.forest.trees().zip(&self.roots).enumerate() {
            let mut cursor = tree.walk();
            let mut native = reference.node().walk();
            let mut ordinal = 0;
            loop {
                check_attributes(
                    cursor.node(),
                    native.node(),
                    &documents[reference.specification.document].source,
                );
                assert_eq!(self.packed_ids[&cursor.node().id()], (tree_index, ordinal));
                assert_eq!(self.native_ids[tree_index][&native.node().id()], ordinal);
                ordinal += 1;
                if native.goto_first_child() {
                    assert!(cursor.goto_first_child());
                    continue;
                }
                assert!(!cursor.goto_first_child());
                while !native.goto_next_sibling() {
                    assert!(!cursor.goto_next_sibling());
                    if !native.goto_parent() {
                        assert!(!cursor.goto_parent());
                        break;
                    }
                    assert!(cursor.goto_parent());
                }
                if native.depth() == 0 {
                    break;
                }
                assert!(cursor.goto_next_sibling());
            }
        }
    }
    fn identity(&self, node: Node<'_>) -> (usize, usize) {
        self.packed_ids[&node.id()]
    }
    fn native_identity(&self, tree: usize, node: tree_sitter::Node<'_>) -> (usize, usize) {
        (tree, self.native_ids[tree][&node.id()])
    }
}

struct SlabOwner {
    words: Box<[u64]>,
    length: usize,
    drops: Arc<AtomicUsize>,
}

// The boxed allocation is aligned, immutable, and stays at the same address until drop.
unsafe impl StableSlab for SlabOwner {
    fn bytes(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.words.as_ptr().cast(), self.length) }
    }
}

impl Drop for SlabOwner {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn retain(bytes: &[u8], counters: &mut Vec<Arc<AtomicUsize>>) -> SlabOwner {
    let mut words = vec![0u64; bytes.len().div_ceil(8)].into_boxed_slice();
    unsafe {
        slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), words.len() * 8)[..bytes.len()]
            .copy_from_slice(bytes);
    }
    let drops = Arc::new(AtomicUsize::new(0));
    counters.push(drops.clone());
    SlabOwner {
        words,
        length: bytes.len(),
        drops,
    }
}

struct ParserValue {
    native: tree_sitter::Parser,
    parse_and_pack: tree_squatter::Parser,
    via_pack: (tree_sitter::Parser, Packer),
    direct: Option<TreeFellerParser>,
}

fn parser(language: usize) -> ParserValue {
    let fixture = &LANGUAGES[language];
    let native = || {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&fixture.native).unwrap();
        parser
    };
    let mut parse_and_pack = tree_squatter::Parser::new();
    parse_and_pack.set_language(&fixture.packed).unwrap();
    let direct = match TreeFellerParser::new(&fixture.packed) {
        Ok(parser) => Some(parser),
        Err(error) => {
            assert_eq!(error.code, Error::Language);
            assert!(fixture.native.abi_version() < 15);
            None
        }
    };
    ParserValue {
        native: native(),
        parse_and_pack,
        via_pack: (native(), Packer::new().unwrap()),
        direct,
    }
}

struct QueryPair {
    packed: Query,
    native: tree_sitter::Query,
    source: &'static str,
}

fn query(language: usize, source: usize) -> QueryPair {
    let fixture = &LANGUAGES[language];
    let source = QUERIES[source];
    let pair = QueryPair {
        packed: Query::new(&fixture.packed, source).unwrap(),
        native: tree_sitter::Query::new(&fixture.native, source).unwrap(),
        source,
    };
    check_query(&pair);
    pair
}

fn check_query(query: &QueryPair) {
    assert_eq!(query.packed.pattern_count(), query.native.pattern_count());
    assert_eq!(query.packed.capture_names(), query.native.capture_names());
    for (index, name) in query.packed.capture_names().iter().enumerate() {
        assert_eq!(
            query.packed.capture_index_for_name(name).unwrap().ix(),
            index
        );
        assert_eq!(
            query.native.capture_index_for_name(name).unwrap() as usize,
            index
        );
    }
    for index in 0..query.packed.pattern_count() {
        let pattern = PatternIx(index);
        assert_eq!(
            query.packed.start_byte_for_pattern(pattern),
            query.native.start_byte_for_pattern(index)
        );
        assert_eq!(
            query.packed.end_byte_for_pattern(pattern),
            query.native.end_byte_for_pattern(index)
        );
        assert_eq!(
            query.packed.capture_quantifiers(pattern),
            query.native.capture_quantifiers(index)
        );
        assert_eq!(
            query.packed.is_pattern_rooted(pattern),
            query.native.is_pattern_rooted(index)
        );
        assert_eq!(
            query.packed.is_pattern_non_local(pattern),
            query.native.is_pattern_non_local(index)
        );
    }
}

struct CursorPair {
    packed: [QueryCursor; 2],
    native: tree_sitter::QueryCursor,
}

impl CursorPair {
    fn new() -> Self {
        let mut general = QueryCursor::new();
        general.set_optimized(false);
        Self {
            packed: [general, QueryCursor::new()],
            native: tree_sitter::QueryCursor::new(),
        }
    }
}

struct Saved {
    metadata: SavedMetadata,
    bytes: Vec<u8>,
}

struct World {
    model: Model,
    parsers: Vec<ParserValue>,
    packers: Vec<Packer>,
    forests: Vec<ForestEntry>,
    saved: Vec<Saved>,
    queries: Vec<QueryPair>,
    cursors: Vec<CursorPair>,
    counters: Vec<Arc<AtomicUsize>>,
}

impl World {
    fn new(documents: Vec<Document>) -> Self {
        let model = Model::new(documents);
        let native =
            support::parse_native(&LANGUAGES[0].native, model.documents[0].source.as_bytes());
        let forest = Forest::pack(&LANGUAGES[0].packed, &native).unwrap();
        let roots = vec![ReferenceRoot {
            specification: model.forests[0].regions[0][0].clone(),
            tree: native,
            path: Vec::new(),
        }];
        let entry = ForestEntry::new(forest, model.forests[0].clone(), roots, &model.documents);
        Self {
            model,
            parsers: vec![parser(0)],
            packers: vec![Packer::new().unwrap()],
            forests: vec![entry],
            saved: Vec::new(),
            queries: Vec::new(),
            cursors: Vec::new(),
            counters: Vec::new(),
        }
    }
    fn reference(&self, specification: &RootSpecification) -> ReferenceRoot {
        let document = &self.model.documents[specification.document];
        let tree = support::parse_native(
            &LANGUAGES[document.language].native,
            document.source.as_bytes(),
        );
        let mut path = Vec::new();
        if specification.subtree != 0 {
            let count = tree.root_node().child_count();
            if count != 0 {
                path.push((specification.subtree as usize - 1) % count as usize);
            }
        }
        ReferenceRoot {
            specification: specification.clone(),
            tree,
            path,
        }
    }
    fn check_pools(&self) {
        assert!(self.model.within_budget(), "runtime pool budget exceeded");
        assert_eq!(self.parsers.len(), self.model.parsers.len());
        assert_eq!(self.packers.len(), self.model.packers);
        assert_eq!(
            self.forests
                .iter()
                .map(|entry| &entry.metadata)
                .collect::<Vec<_>>(),
            self.model.forests.iter().collect::<Vec<_>>()
        );
        assert_eq!(
            self.saved
                .iter()
                .map(|saved| &saved.metadata)
                .collect::<Vec<_>>(),
            self.model.saved.iter().collect::<Vec<_>>()
        );
        assert_eq!(self.queries.len(), self.model.queries.len());
        assert_eq!(self.cursors.len(), self.model.cursors.len());
        for counter in &self.counters {
            let live = self.forests.iter().any(|entry| {
                entry
                    .owners
                    .iter()
                    .flatten()
                    .any(|owner| Arc::ptr_eq(owner, counter))
            });
            assert_eq!(
                counter.load(Ordering::SeqCst),
                usize::from(!live),
                "retained owner lifetime"
            );
        }
    }
    fn step(&mut self, operation: &Operation, coverage: &mut Coverage) {
        let mut next = self.model.clone();
        next.apply(operation);
        match operation {
            Operation::Add { .. } | Operation::Splice { .. } => {}
            Operation::NewParser { language } => self.parsers.push(parser(*language)),
            Operation::ParserScratch(index) => {
                let parser = &mut self.parsers[*index];
                parser.parse_and_pack.drop_scratch();
                parser.via_pack.1.drop_scratch();
                if let Some(direct) = &mut parser.direct {
                    direct.drop_scratch();
                }
            }
            Operation::ResetParser(index) => {
                let parser = &mut self.parsers[*index];
                parser.native.reset();
                parser.parse_and_pack.reset();
                parser.via_pack.0.reset();
            }
            Operation::DropParser(index) => {
                self.parsers.remove(*index);
            }
            Operation::NewPacker => self.packers.push(Packer::new().unwrap()),
            Operation::PackerScratch(index) => self.packers[*index].drop_scratch(),
            Operation::DropPacker(index) => {
                self.packers.remove(*index);
            }
            Operation::Parse {
                parser,
                document,
                options,
                chunk,
            } => {
                let document = &self.model.documents[*document];
                let source = document.source.as_bytes();
                let selection = |region: tree_squatter::ForestRegion<'_>| {
                    options.presence.selects(region.index().ix())
                };
                let pack = options.pack(&selection);
                let mut callback = |byte: usize, _: Point| {
                    &source[byte..if *chunk == 0 {
                        source.len()
                    } else {
                        (byte + *chunk as usize).min(source.len())
                    }]
                };
                let parser = &mut self.parsers[*parser];
                let native = Parse::parse_with_options(
                    &mut parser.native,
                    &mut callback,
                    Default::default(),
                )
                .unwrap();
                coverage.parses[0] += 1;
                let forest = parser
                    .parse_and_pack
                    .parse_with_options(
                        &mut callback,
                        PackedParseOptions {
                            pack,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                coverage.parses[1] += 1;
                let via_pack = Parse::parse_with_options(
                    &mut parser.via_pack.0,
                    &mut callback,
                    Default::default(),
                )
                .unwrap();
                let packed = parser
                    .via_pack
                    .1
                    .pack_with_options(&LANGUAGES[document.language].packed, &via_pack, pack)
                    .unwrap();
                coverage.parses[2] += 1;
                support::assert_same_tree(&forest, &packed);
                let expected =
                    Forest::pack_with_options(&LANGUAGES[document.language].packed, &native, pack)
                        .unwrap();
                support::assert_same_tree(&forest, &expected);
                if let Some(direct) = &mut parser.direct {
                    coverage.parses[3] += 1;
                    match direct.parse_with_options(
                        &mut callback,
                        PackedParseOptions {
                            pack,
                            ..Default::default()
                        },
                    ) {
                        Ok(direct) => support::assert_same_tree(&forest, &direct),
                        Err(error) => {
                            assert_eq!(error.code, Error::Parse);
                            coverage.direct_failures += 1;
                        }
                    }
                }
                let metadata = next.forests.last().unwrap().clone();
                let reference = ReferenceRoot {
                    specification: metadata.regions[0][0].clone(),
                    tree: native,
                    path: Vec::new(),
                };
                self.forests.push(ForestEntry::new(
                    forest,
                    metadata,
                    vec![reference],
                    &self.model.documents,
                ));
            }
            Operation::Pack {
                packer,
                options,
                mixed,
                ..
            } => {
                let metadata = next.forests.last().unwrap().clone();
                let roots: Vec<_> = metadata
                    .regions
                    .iter()
                    .flatten()
                    .map(|specification| self.reference(specification))
                    .collect();
                let inputs = || {
                    let mut offset = 0;
                    metadata
                        .regions
                        .iter()
                        .map(|region| {
                            let input = PackRegion {
                                language: LANGUAGES
                                    [self.model.documents[region[0].document].language]
                                    .packed
                                    .clone(),
                                roots: roots[offset..offset + region.len()]
                                    .iter()
                                    .map(ReferenceRoot::node)
                                    .collect(),
                            };
                            offset += region.len();
                            input
                        })
                        .collect()
                };
                let selection = |region: tree_squatter::ForestRegion<'_>| {
                    options.presence.selects(region.index().ix())
                };
                let (forest, mapping) = self.packers[*packer]
                    .pack_forest(inputs(), options.pack(&selection))
                    .unwrap();
                assert_eq!(
                    mapping,
                    (0..roots.len())
                        .map(|index| TreeIx::from_raw(index as u32))
                        .collect::<Vec<_>>()
                );
                let (fresh, _) = Packer::new()
                    .unwrap()
                    .pack_forest(inputs(), options.pack(&selection))
                    .unwrap();
                support::assert_same_tree(&forest, &fresh);
                coverage.mixed += usize::from(*mixed);
                self.forests.push(ForestEntry::new(
                    forest,
                    metadata,
                    roots,
                    &self.model.documents,
                ));
            }
            Operation::InvalidParse(index) => {
                let language = self.model.parsers[*index];
                let parser = self.parsers[*index].direct.as_mut().unwrap();
                assert_eq!(
                    parser.parse(LANGUAGES[language].invalid).unwrap_err().code,
                    Error::Parse
                );
                let forest = parser.parse(LANGUAGES[language].sources[0]).unwrap();
                let native = support::parse_native(
                    &LANGUAGES[language].native,
                    LANGUAGES[language].sources[0],
                );
                support::assert_same_tree(
                    &forest,
                    &Forest::pack(&LANGUAGES[language].packed, &native).unwrap(),
                );
                coverage.failures += 1;
            }
            Operation::InvalidRegion(index) => {
                assert_eq!(
                    self.packers[*index]
                        .pack_forest(
                            vec![PackRegion {
                                language: LANGUAGES[0].packed.clone(),
                                roots: Vec::new()
                            }],
                            PackOptions::default()
                        )
                        .unwrap_err(),
                    Error::InvalidArgument
                );
                let native = support::parse_native(&LANGUAGES[0].native, "[1]");
                let forest = self.packers[*index]
                    .pack(&LANGUAGES[0].packed, &native)
                    .unwrap();
                support::assert_same_tree(
                    &forest,
                    &Forest::pack(&LANGUAGES[0].packed, &native).unwrap(),
                );
                coverage.failures += 1;
            }
            Operation::CancelParse(index) => {
                let language = self.model.parsers[*index];
                let source = LANGUAGES[language].sources[0].as_bytes();
                let calls = Cell::new(0);
                let mut stop = |_: &dyn ParseStateLike| {
                    calls.set(calls.get() + 1);
                    ControlFlow::Break(())
                };
                let parser = &mut self.parsers[*index].parse_and_pack;
                let result = parser.parse_with_options(
                    &mut |byte, _| &source[byte..],
                    tree_squatter::ParseOptions::new()
                        .progress_callback(&mut stop)
                        .into(),
                );
                if calls.get() != 0 {
                    assert_eq!(result.unwrap_err(), tree_squatter::ParserError::Canceled);
                } else {
                    result.unwrap().validate().unwrap();
                }
                parser.reset();
                let forest = parser.parse(source).unwrap();
                let native = support::parse_native(&LANGUAGES[language].native, source);
                support::assert_same_tree(
                    &forest,
                    &Forest::pack(&LANGUAGES[language].packed, &native).unwrap(),
                );
            }
            Operation::CancelPresence(index) => {
                let entry = &self.forests[*index];
                let before = entry
                    .forest
                    .presence_cache()
                    .map(|cache| cache.as_bytes().to_vec());
                let calls = Cell::new(0);
                let result = PresenceCache::build_selected_with_cancellation(
                    &entry.forest,
                    |_| true,
                    || {
                        calls.set(calls.get() + 1);
                        ControlFlow::Break(())
                    },
                );
                if calls.get() != 0 {
                    assert!(matches!(
                        result,
                        Err(tree_squatter::SideDataError::Core(Error::Canceled))
                    ));
                } else {
                    result.unwrap().validate_for(&entry.forest).unwrap();
                }
                assert_eq!(
                    before.as_deref(),
                    entry.forest.presence_cache().map(PresenceCache::as_bytes)
                );
                PresenceCache::build(&entry.forest)
                    .unwrap()
                    .validate_for(&entry.forest)
                    .unwrap();
            }
            Operation::Copy { forest, mode } => {
                let source = &self.forests[*forest];
                let languages: Vec<_> = source
                    .forest
                    .regions()
                    .map(|region| region.language().clone())
                    .collect();
                let mut owner = None;
                let forest = match mode {
                    CopyMode::Compact => source.forest.to_compacted().unwrap(),
                    CopyMode::Detach => source.forest.detach().unwrap(),
                    CopyMode::Load => {
                        Forest::from_bytes(&languages, source.forest.as_bytes()).unwrap()
                    }
                    CopyMode::SafetyChecked => {
                        Forest::from_bytes_safety_checked(&languages, source.forest.as_bytes())
                            .unwrap()
                    }
                    // These bytes and bindings came directly from a live, validated forest.
                    CopyMode::TrustedLoad => unsafe {
                        Forest::from_bytes_unchecked(&languages, source.forest.as_bytes())
                    }
                    .unwrap(),
                    CopyMode::Retain | CopyMode::TrustedRetain => {
                        let slab = retain(source.forest.as_bytes(), &mut self.counters);
                        let address = slab.bytes().as_ptr();
                        owner = Some(slab.drops.clone());
                        let retained = if *mode == CopyMode::Retain {
                            Forest::from_retained(&languages, slab)
                        } else {
                            unsafe { Forest::from_retained_unchecked(&languages, slab) }
                        }
                        .unwrap();
                        assert_eq!(retained.as_bytes().as_ptr(), address);
                        retained
                    }
                };
                let mut entry = ForestEntry::new(
                    forest,
                    next.forests.last().unwrap().clone(),
                    source.roots.clone(),
                    &self.model.documents,
                );
                assert_eq!(entry.packed_ids, source.packed_ids);
                entry.owners[0] = owner;
                coverage.copies[*mode as usize] += 1;
                self.forests.push(entry);
            }
            Operation::Compact(index) => {
                let entry = &mut self.forests[*index];
                entry.forest.compact().unwrap();
                entry.owners[0] = None;
                entry.metadata = next.forests[*index].clone();
                entry.check(&self.model.documents);
            }
            Operation::DropForest(index) => {
                self.forests.remove(*index);
            }
            Operation::Save { forest, side } => {
                let entry = &self.forests[*forest];
                let bytes = match side {
                    Side::Points => entry.forest.point_data().unwrap().as_bytes(),
                    Side::Presence => entry.forest.presence_cache().unwrap().as_bytes(),
                };
                assert!(bytes.len() <= 1024 * 1024);
                self.saved.push(Saved {
                    metadata: next.saved.last().unwrap().clone(),
                    bytes: bytes.to_vec(),
                });
            }
            Operation::Restore {
                forest,
                saved,
                retained,
            } => {
                let saved = &self.saved[*saved];
                let entry = &mut self.forests[*forest];
                let mut owner = None;
                let slab = if *retained {
                    let slab = retain(&saved.bytes, &mut self.counters);
                    owner = Some(slab.drops.clone());
                    Some(slab)
                } else {
                    None
                };
                match saved.metadata.side {
                    Side::Points => {
                        let points = if let Some(slab) = slab {
                            PointsData::from_retained(slab)
                        } else {
                            PointsData::copy_from_bytes(&entry.forest, &saved.bytes)
                        }
                        .unwrap();
                        points.validate_for(&entry.forest).unwrap();
                        entry.forest.set_point_data(points).unwrap();
                        entry.owners[1] = owner;
                    }
                    Side::Presence => {
                        let cache = if let Some(slab) = slab {
                            PresenceCache::from_retained(slab)
                        } else {
                            PresenceCache::copy_from_bytes(&entry.forest, &saved.bytes)
                        }
                        .unwrap();
                        cache.validate_for(&entry.forest).unwrap();
                        entry.forest.set_presence_cache(cache).unwrap();
                        entry.owners[2] = owner;
                    }
                }
                entry.metadata = next.forests[*forest].clone();
                entry.check(&self.model.documents);
            }
            Operation::DropSaved(index) => {
                self.saved.remove(*index);
            }
            Operation::DropSide { forest, side } => {
                let entry = &mut self.forests[*forest];
                match side {
                    Side::Points => {
                        entry.forest.drop_point_data();
                        entry.owners[1] = None;
                    }
                    Side::Presence => {
                        entry.forest.drop_presence_cache();
                        entry.owners[2] = None;
                    }
                }
                entry.metadata = next.forests[*forest].clone();
                entry.check(&self.model.documents);
            }
            Operation::BuildPresence { forest, presence } => {
                let entry = &mut self.forests[*forest];
                let cache = PresenceCache::build_selected(&entry.forest, |region| {
                    presence.selects(region.index().ix())
                })
                .unwrap();
                cache.validate_for(&entry.forest).unwrap();
                entry.forest.set_presence_cache(cache).unwrap();
                entry.owners[2] = None;
                entry.metadata = next.forests[*forest].clone();
                entry.check(&self.model.documents);
            }
            Operation::InvalidSide(index) => {
                let entry = &mut self.forests[*index];
                let (points, presence) = (
                    entry
                        .forest
                        .point_data()
                        .map(|data| data.as_bytes().to_vec()),
                    entry
                        .forest
                        .presence_cache()
                        .map(|data| data.as_bytes().to_vec()),
                );
                let wrong = if entry.roots.is_empty() {
                    let native = support::parse_native(&LANGUAGES[0].native, "[1]");
                    Forest::pack(&LANGUAGES[0].packed, &native).unwrap()
                } else {
                    Packer::new()
                        .unwrap()
                        .pack_forest(Vec::new(), PackOptions::default())
                        .unwrap()
                        .0
                };
                // Empty and nonempty forests always have different sidecar dimensions.
                let cache = PresenceCache::build(&wrong).unwrap();
                assert!(entry.forest.set_presence_cache(cache).is_err());
                let data = PointsData::from_bytes(wrong.point_data().unwrap().as_bytes()).unwrap();
                assert!(entry.forest.set_point_data(data).is_err());
                assert_eq!(
                    points.as_deref(),
                    entry.forest.point_data().map(PointsData::as_bytes)
                );
                assert_eq!(
                    presence.as_deref(),
                    entry.forest.presence_cache().map(PresenceCache::as_bytes)
                );
                coverage.failures += 1;
            }
            Operation::NewQuery { language, source } => {
                self.queries.push(query(*language, *source))
            }
            Operation::CloneQuery(index) => {
                let query = &self.queries[*index];
                self.queries.push(QueryPair {
                    packed: query.packed.deep_clone(),
                    native: query.native.deep_clone(),
                    source: query.source,
                });
            }
            Operation::DropQuery(index) => {
                self.queries.remove(*index);
            }
            Operation::DisablePattern { query, pattern } => {
                self.queries[*query]
                    .packed
                    .disable_pattern(PatternIx(*pattern));
                self.queries[*query].native.disable_pattern(*pattern);
            }
            Operation::DisableCapture { query, capture } => {
                let name = capture_names(self.model.queries[*query].source)[*capture];
                self.queries[*query].packed.disable_capture(name);
                self.queries[*query].native.disable_capture(name);
            }
            Operation::InvalidQuery(language) => {
                let native =
                    tree_sitter::Query::new(&LANGUAGES[*language].native, "(((").unwrap_err();
                let packed = Query::new(&LANGUAGES[*language].packed, "(((")
                    .err()
                    .unwrap();
                assert_eq!(
                    (packed.kind, packed.row, packed.column, packed.offset),
                    (native.kind, native.row, native.column, native.offset)
                );
                coverage.failures += 1;
            }
            Operation::NewQueryCursor => self.cursors.push(CursorPair::new()),
            Operation::DropQueryCursor(index) => {
                self.cursors.remove(*index);
            }
            Operation::Configure { .. } => {}
            Operation::Execute {
                forest,
                region,
                query,
                cursor,
                script,
                scope,
            } => run_query(
                &self.forests[*forest],
                &self.model.documents,
                &self.queries[*query],
                &mut self.cursors[*cursor],
                QueryRecipe {
                    region: *region,
                    config: &self.model.cursors[*cursor],
                    script: *script,
                    scope: *scope,
                },
                coverage,
            ),
            Operation::WrongGrammar {
                forest,
                query,
                cursor,
            } => {
                let root = self.forests[*forest]
                    .forest
                    .trees()
                    .next()
                    .unwrap()
                    .root_node();
                for cursor in &mut self.cursors[*cursor].packed {
                    let mut execution =
                        cursor.execute(&self.queries[*query].packed, root, b"" as &[u8]);
                    assert_eq!(
                        execution.error(),
                        Some(QueryExecutionError::InvalidExecution)
                    );
                    assert!(execution.next_match().is_none());
                }
                coverage.failures += 1;
            }
            Operation::Explore(operations) => {
                explore(&self.forests, &self.model.documents, operations, coverage)
            }
        }
        self.model = next;
        self.check_pools();
        coverage.executed += 1;
    }
}

#[derive(Clone, Copy)]
struct NodePair<'a> {
    packed: Node<'a>,
    native: tree_sitter::Node<'a>,
    forest: usize,
    tree: usize,
}

struct CursorView<'a> {
    packed: tree_squatter::TreeCursor<'a>,
    native: tree_sitter::TreeCursor<'a>,
    forest: usize,
    tree: usize,
}

impl<'a> CursorView<'a> {
    fn node(&self) -> NodePair<'a> {
        NodePair {
            packed: self.packed.node(),
            native: self.native.node(),
            forest: self.forest,
            tree: self.tree,
        }
    }
    fn check(&mut self, entries: &[ForestEntry], documents: &[Document]) {
        check_pair(self.node(), entries, documents);
        assert_eq!(self.packed.attributes(), self.packed.node().attributes());
        assert_eq!(self.packed.depth(), self.native.depth());
        assert_eq!(
            self.packed.field_id().map(FieldId::raw),
            self.native.field_id().map(|field| field.get())
        );
        assert_eq!(self.packed.field_name(), self.native.field_name());
    }
}

fn point(source: &str, byte: usize) -> Point {
    let prefix = &source.as_bytes()[..byte.min(source.len())];
    let row = prefix.iter().filter(|&&byte| byte == b'\n').count();
    let column = prefix
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(prefix.len(), |position| prefix.len() - position - 1)
        + byte.saturating_sub(source.len());
    Point::new(row, column)
}

fn check_pair(pair: NodePair<'_>, entries: &[ForestEntry], documents: &[Document]) {
    let entry = &entries[pair.forest];
    assert_eq!(
        entry.identity(pair.packed),
        entry.native_identity(pair.tree, pair.native)
    );
    check_attributes(
        pair.packed,
        pair.native,
        &documents[entry.roots[pair.tree].specification.document].source,
    );
}

fn navigation<'a>(
    pair: NodePair<'a>,
    operation: &Navigation,
    entry: &ForestEntry,
    source: &str,
    coverage: &mut Coverage,
) -> NodePair<'a> {
    let (packed, native) = (pair.packed, pair.native);
    let resolved = match operation {
        Navigation::Child(child) => {
            format!("child={}", child.resolve(native.child_count() as usize))
        }
        Navigation::NamedChild(child) => {
            format!("named child={}", child.resolve(native.named_child_count()))
        }
        Navigation::FirstByte(position) | Navigation::FirstNamedByte(position) => {
            format!("byte={}", position.resolve(native))
        }
        Navigation::Descendant(start, end, _, _) => {
            format!("range={}..{}", start.resolve(native), end.resolve(native))
        }
        _ => String::new(),
    };
    let (actual, expected) = match *operation {
        Navigation::Parent => (packed.parent(), native.parent()),
        Navigation::Child(child) => {
            let index = child.resolve(native.child_count() as usize);
            (
                packed.child(ChildIx(index as u32)),
                native.child(index as u32),
            )
        }
        Navigation::NamedChild(child) => {
            let index = child.resolve(native.named_child_count());
            (
                packed.named_child(NamedChildIx(index as u32)),
                native.named_child(index as u32),
            )
        }
        Navigation::Next => (packed.next_sibling(), native.next_sibling()),
        Navigation::Previous => (packed.prev_sibling(), native.prev_sibling()),
        Navigation::NextNamed => (packed.next_named_sibling(), native.next_named_sibling()),
        Navigation::PreviousNamed => (packed.prev_named_sibling(), native.prev_named_sibling()),
        Navigation::Field(index) => {
            let name = ["key", "value", "name", "body", "left", "right", "missing"][index % 7];
            let actual = packed.child_by_field_name(name);
            if let Some(field) = packed.language().field_id_for_name(name.as_bytes()) {
                assert_eq!(packed.child_by_field_id(field), actual);
            }
            (actual, native.child_by_field_name(name))
        }
        Navigation::FirstByte(position) => {
            let byte = position.resolve(native);
            (
                packed.first_child_for_byte(byte),
                native.first_child_for_byte(byte),
            )
        }
        Navigation::FirstNamedByte(position) => {
            let byte = position.resolve(native);
            (
                packed.first_named_child_for_byte(byte),
                native.first_named_child_for_byte(byte),
            )
        }
        Navigation::Descendant(start, end, named, points) => {
            let (start, end) = (start.resolve(native), end.resolve(native));
            eprintln_if_verbose(format_args!(
                "range={start}..{end}, named={named}, points={points}"
            ));
            if points {
                let (start_point, end_point) = if packed.has_points() {
                    (point(source, start), point(source, end))
                } else {
                    (Point::new(0, start), Point::new(0, end))
                };
                let actual = if named {
                    packed.named_descendant_for_point_range(start_point, end_point)
                } else {
                    packed.descendant_for_point_range(start_point, end_point)
                };
                let expected = if !packed.has_points() {
                    if named {
                        native.named_descendant_for_byte_range(start, end)
                    } else {
                        native.descendant_for_byte_range(start, end)
                    }
                } else if named {
                    native.named_descendant_for_point_range(start_point, end_point)
                } else {
                    native.descendant_for_point_range(start_point, end_point)
                };
                (actual, expected)
            } else {
                (
                    if named {
                        packed.named_descendant_for_byte_range(start, end)
                    } else {
                        packed.descendant_for_byte_range(start, end)
                    },
                    if named {
                        native.named_descendant_for_byte_range(start, end)
                    } else {
                        native.descendant_for_byte_range(start, end)
                    },
                )
            }
        }
    };
    let expected = expected.filter(|node| entry.native_ids[pair.tree].contains_key(&node.id()));
    assert_eq!(
        actual.map(|node| entry.identity(node)),
        expected.map(|node| entry.native_identity(pair.tree, node)),
        "{operation:?}, {resolved}, input={:?}",
        entry.identity(packed)
    );
    coverage.navigation[usize::from(actual.is_some())] += 1;
    NodePair {
        packed: actual.unwrap_or(packed),
        native: expected.unwrap_or(native),
        ..pair
    }
}

fn eprintln_if_verbose(arguments: std::fmt::Arguments<'_>) {
    if std::env::var_os("BISIM_TRACE").is_some() {
        eprintln!("{arguments}");
    }
}

fn explore(
    entries: &[ForestEntry],
    documents: &[Document],
    operations: &[ViewOperation],
    coverage: &mut Coverage,
) {
    let borrowed: Vec<_> = entries
        .iter()
        .map(|entry| {
            let languages: Vec<_> = entry
                .forest
                .regions()
                .map(|region| region.language().clone())
                .collect();
            let borrowed =
                Forest::from_bytes_borrowed(&languages, entry.forest.as_bytes()).unwrap();
            assert_eq!(
                borrowed.as_bytes().as_ptr(),
                entry.forest.as_bytes().as_ptr()
            );
            borrowed.validate().unwrap();
            borrowed
        })
        .collect();
    let mut nodes = Vec::new();
    let mut cursors = Vec::new();
    for (forest, entry) in entries.iter().enumerate() {
        for forest_view in [&entry.forest, &*borrowed[forest]] {
            for (tree, (packed, native)) in forest_view.trees().zip(&entry.roots).enumerate() {
                let pair = NodePair {
                    packed: packed.root_node(),
                    native: native.node(),
                    forest,
                    tree,
                };
                nodes.push(pair);
                cursors.push(CursorView {
                    packed: pair.packed.walk(),
                    native: pair.native.walk(),
                    forest,
                    tree,
                });
            }
        }
    }
    let (mut node_count, mut cursor_count) = (nodes.len(), cursors.len());
    for (index, operation) in operations.iter().enumerate() {
        let result = catch_unwind(AssertUnwindSafe(|| {
            match operation {
                ViewOperation::Read(index) => {
                    let pair = nodes[*index];
                    check_pair(pair, entries, documents);
                    let entry = &entries[pair.forest];
                    let mut packed_cursor = pair.packed.walk();
                    let mut native_cursor = pair.native.walk();
                    let actual: Vec<_> = pair
                        .packed
                        .children(&mut packed_cursor)
                        .map(|node| entry.identity(node))
                        .collect();
                    let expected: Vec<_> = pair
                        .native
                        .children(&mut native_cursor)
                        .map(|node| entry.native_identity(pair.tree, node))
                        .collect();
                    assert_eq!(actual, expected);
                    let actual: Vec<_> = pair
                        .packed
                        .named_children(&mut packed_cursor)
                        .map(|node| entry.identity(node))
                        .collect();
                    let expected: Vec<_> = pair
                        .native
                        .named_children(&mut native_cursor)
                        .map(|node| entry.native_identity(pair.tree, node))
                        .collect();
                    assert_eq!(actual, expected);
                    let expected_field = pair
                        .native
                        .parent()
                        .filter(|parent| entry.native_ids[pair.tree].contains_key(&parent.id()))
                        .and_then(|parent| {
                            (0..parent.child_count())
                                .find(|&index| parent.child(index) == Some(pair.native))
                                .and_then(|index| parent.field_name_for_child(index))
                        });
                    assert_eq!(pair.packed.field_name(), expected_field);
                    assert_eq!(
                        pair.packed.field_id(),
                        expected_field.and_then(|name| pair
                            .packed
                            .language()
                            .field_id_for_name(name.as_bytes()))
                    );
                }
                ViewOperation::Navigate {
                    node,
                    navigation: operation,
                } => {
                    let pair = nodes[*node];
                    let entry = &entries[pair.forest];
                    let source = &documents[entry.roots[pair.tree].specification.document].source;
                    nodes.push(navigation(pair, operation, entry, source, coverage));
                    node_count += 1;
                }
                ViewOperation::DropNode(index) => {
                    nodes.remove(*index);
                    node_count -= 1;
                }
                ViewOperation::NewCursor(index) => {
                    let pair = nodes[*index];
                    cursors.push(CursorView {
                        packed: pair.packed.walk(),
                        native: pair.native.walk(),
                        forest: pair.forest,
                        tree: pair.tree,
                    });
                    cursor_count += 1;
                }
                ViewOperation::CloneCursor(index) => {
                    let cursor = &cursors[*index];
                    cursors.push(CursorView {
                        packed: cursor.packed.clone(),
                        native: cursor.native.clone(),
                        forest: cursor.forest,
                        tree: cursor.tree,
                    });
                    cursor_count += 1;
                }
                ViewOperation::DropCursor(index) => {
                    cursors.remove(*index);
                    cursor_count -= 1;
                }
                ViewOperation::ReadCursor(index) => {
                    cursors[*index].check(entries, documents);
                    nodes.push(cursors[*index].node());
                    node_count += 1;
                }
                ViewOperation::Move {
                    cursor,
                    movement,
                    position,
                } => {
                    let cursor = &mut cursors[*cursor];
                    let byte = position.resolve(cursor.native.node());
                    match movement % 7 {
                        0 => assert_eq!(
                            cursor.packed.goto_first_child(),
                            cursor.native.goto_first_child()
                        ),
                        1 => assert_eq!(
                            cursor.packed.goto_last_child(),
                            cursor.native.goto_last_child()
                        ),
                        2 => assert_eq!(cursor.packed.goto_parent(), cursor.native.goto_parent()),
                        3 => assert_eq!(
                            cursor.packed.goto_next_sibling(),
                            cursor.native.goto_next_sibling()
                        ),
                        4 => assert_eq!(
                            cursor.packed.goto_previous_sibling(),
                            cursor.native.goto_previous_sibling()
                        ),
                        5 => assert_eq!(
                            cursor
                                .packed
                                .goto_first_child_for_byte(byte)
                                .map(ChildIx::ix),
                            cursor.native.goto_first_child_for_byte(byte)
                        ),
                        6 => {
                            let source = &documents[entries[cursor.forest].roots[cursor.tree]
                                .specification
                                .document]
                                .source;
                            let expected = if cursor.packed.node().has_points() {
                                cursor
                                    .native
                                    .goto_first_child_for_point(point(source, byte))
                            } else {
                                cursor.native.goto_first_child_for_byte(byte)
                            };
                            let point = if cursor.packed.node().has_points() {
                                point(source, byte)
                            } else {
                                Point::new(0, byte)
                            };
                            assert_eq!(
                                cursor
                                    .packed
                                    .goto_first_child_for_point(point)
                                    .map(ChildIx::ix),
                                expected
                            );
                        }
                        _ => unreachable!(),
                    }
                    cursor.check(entries, documents);
                }
                ViewOperation::Reset { cursor, node } => {
                    let pair = nodes[*node];
                    let cursor = &mut cursors[*cursor];
                    cursor.packed.reset(pair.packed);
                    cursor.native.reset(pair.native);
                    cursor.forest = pair.forest;
                    cursor.tree = pair.tree;
                    cursor.check(entries, documents);
                }
                ViewOperation::ResetTo { cursor, other } => {
                    let packed = cursors[*other].packed.clone();
                    let native = cursors[*other].native.clone();
                    let (forest, tree) = (cursors[*other].forest, cursors[*other].tree);
                    let cursor = &mut cursors[*cursor];
                    cursor.packed.reset_to(&packed);
                    cursor.native.reset_to(&native);
                    cursor.forest = forest;
                    cursor.tree = tree;
                    cursor.check(entries, documents);
                }
                ViewOperation::Scan { node, recipe } => run_scan(
                    nodes[*node],
                    &entries[nodes[*node].forest],
                    documents,
                    *recipe,
                    coverage,
                ),
            }
            assert_eq!(nodes.len(), node_count);
            assert_eq!(cursors.len(), cursor_count);
            for cursor in &mut cursors {
                cursor.check(entries, documents);
            }
            coverage.views += 1;
        }));
        if let Err(error) = result {
            panic!("view step {index}: {operation:?}: {}", panic_message(error));
        }
    }
}

fn native_order(root: tree_sitter::Node<'_>, postorder: bool) -> Vec<tree_sitter::Node<'_>> {
    fn visit<'a>(
        node: tree_sitter::Node<'a>,
        postorder: bool,
        nodes: &mut Vec<tree_sitter::Node<'a>>,
    ) {
        if !postorder {
            nodes.push(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(child, postorder, nodes);
        }
        if postorder {
            nodes.push(node);
        }
    }
    let mut nodes = Vec::new();
    visit(root, postorder, &mut nodes);
    nodes
}

fn relation<T: Ord>(start: T, end: T, range: &Range<T>, relation: u8) -> bool {
    match relation % 6 {
        0 => true,
        1 => {
            range.start < range.end
                && start < range.end
                && (end > range.start || start >= range.start)
        }
        2 => range.start <= range.end && start >= range.start && end <= range.end,
        3 => range.start <= range.end && start <= range.start && end >= range.end,
        4 => range.start < range.end && start >= range.start && start < range.end,
        5 => range.start < range.end && end >= range.start && end < range.end,
        _ => unreachable!(),
    }
}

fn check_scan<'a, S: GroupScan<'a>>(
    make: impl Fn() -> Scan<'a, S>,
    entry: &ForestEntry,
    expected: &[(usize, usize)],
    consume: usize,
) {
    let describe = |node| entry.identity(node);
    assert_eq!(make().nodes().map(describe).collect::<Vec<_>>(), expected);
    assert_eq!(make().count(), expected.len());
    assert_eq!(make().rev().count(), expected.len());
    let reversed: Vec<_> = expected.iter().rev().copied().collect();
    assert_eq!(
        make().rev().nodes().map(describe).collect::<Vec<_>>(),
        reversed
    );
    let groups = make()
        .groups()
        .flat_map(|group| {
            assert!(!group.is_empty());
            let nodes: Vec<_> = group.nodes().map(describe).collect();
            assert_eq!(group.len(), nodes.len());
            nodes
        })
        .collect::<Vec<_>>();
    assert_eq!(groups, expected);
    assert_eq!(
        make()
            .rev()
            .groups()
            .flat_map(|group| group.nodes().map(describe))
            .collect::<Vec<_>>(),
        reversed
    );
    let mut nodes = make().nodes();
    for index in 0..consume {
        assert_eq!(nodes.next().map(describe), expected.get(index).copied());
    }
    assert_eq!(nodes.count(), expected.len().saturating_sub(consume));
    let mut nodes = make().nodes();
    assert_eq!(
        nodes.nth(consume).map(describe),
        expected.get(consume).copied()
    );
    assert_eq!(
        nodes.fold(Vec::new(), |mut result, node| {
            result.push(describe(node));
            result
        }),
        expected[expected.len().min(consume + 1)..]
    );
}

fn run_scan(
    pair: NodePair<'_>,
    entry: &ForestEntry,
    documents: &[Document],
    recipe: ScanRecipe,
    coverage: &mut Coverage,
) {
    let source = &documents[entry.roots[pair.tree].specification.document].source;
    let range = recipe.start.resolve(pair.native)..recipe.end.resolve(pair.native);
    let coordinates = |byte| {
        if pair.packed.has_points() {
            point(source, byte)
        } else {
            Point::new(0, byte)
        }
    };
    let point_range = coordinates(range.start)..coordinates(range.end);
    let kind = pair.native.kind_id();
    let field = pair.packed.field_id();
    let native_language = pair.native.language();
    let supertypes = native_language.supertypes();
    let supertype = supertypes
        .get(recipe.consume as usize % supertypes.len().max(1))
        .copied()
        .map(GrammarId::from_raw);
    let mut supertype_nodes = BTreeSet::new();
    if recipe.filter % 6 == 5
        && let Some(supertype) = supertype
    {
        let name = native_language.node_kind_for_id(supertype.raw()).unwrap();
        let query = tree_sitter::Query::new(&native_language, &format!("({name}) @node")).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        // Descendant queries omit hidden ancestors; use the packed root's scope.
        let mut matches = cursor.matches(&query, entry.roots[pair.tree].node(), source.as_bytes());
        while let Some(found) = matches.next() {
            supertype_nodes.extend(found.captures().iter().map(|capture| capture.node.id()));
        }
    }
    let expected: Vec<_> = native_order(pair.native, recipe.postorder)
        .into_iter()
        .filter(|node| {
            let selected = if recipe.points {
                let (start, end) = if pair.packed.has_points() {
                    (node.start_position(), node.end_position())
                } else {
                    (
                        Point::new(0, node.start_byte()),
                        Point::new(0, node.end_byte()),
                    )
                };
                relation(start, end, &point_range, recipe.relation)
            } else {
                relation(node.start_byte(), node.end_byte(), &range, recipe.relation)
            };
            selected
                && match recipe.filter % 6 {
                    0 => true,
                    1 => node.kind_id() == kind,
                    2 => !node.is_extra(),
                    3 => !node.is_missing(),
                    4 => {
                        let name = node
                            .parent()
                            .filter(|parent| entry.native_ids[pair.tree].contains_key(&parent.id()))
                            .and_then(|parent| {
                                (0..parent.child_count())
                                    .find(|&index| parent.child(index) == Some(*node))
                                    .and_then(|index| parent.field_name_for_child(index))
                            });
                        name.and_then(|name| {
                            pair.packed.language().field_id_for_name(name.as_bytes())
                        }) == field
                    }
                    5 => supertype_nodes.contains(&node.id()),
                    _ => unreachable!(),
                }
        })
        .map(|node| entry.native_identity(pair.tree, node))
        .collect();
    let expected: Vec<_> = if recipe.reverse {
        expected.into_iter().rev().collect()
    } else {
        expected
    };
    macro_rules! filtered {
        ($make:expr) => {
            match recipe.filter % 6 {
                0 => check_scan($make, entry, &expected, recipe.consume as usize),
                1 => check_scan(
                    || ($make)().filter_kind_ids([tree_squatter::KindId::from_raw(kind)]),
                    entry,
                    &expected,
                    recipe.consume as usize,
                ),
                2 => check_scan(
                    || ($make)().filter_extra(false),
                    entry,
                    &expected,
                    recipe.consume as usize,
                ),
                3 => check_scan(
                    || ($make)().filter_missing(false),
                    entry,
                    &expected,
                    recipe.consume as usize,
                ),
                4 => check_scan(
                    || ($make)().filter_field_id(field),
                    entry,
                    &expected,
                    recipe.consume as usize,
                ),
                5 => check_scan(
                    || {
                        ($make)()
                            .filter_supertype_id(supertype.unwrap_or(GrammarId::from_raw(u16::MAX)))
                    },
                    entry,
                    &expected,
                    recipe.consume as usize,
                ),
                _ => unreachable!(),
            }
        };
    }
    macro_rules! ranged {
        ($make:expr) => {
            match (recipe.points, recipe.relation % 6) {
                (_, 0) => filtered!($make),
                (false, 1) => filtered!(|| ($make)().overlapping_bytes(range.clone())),
                (false, 2) => filtered!(|| ($make)().within_bytes(range.clone())),
                (false, 3) => filtered!(|| ($make)().containing_bytes(range.clone())),
                (false, 4) => filtered!(|| ($make)().starting_in_bytes(range.clone())),
                (false, 5) => filtered!(|| ($make)().ending_in_bytes(range.clone())),
                (true, 1) => filtered!(|| ($make)().overlapping_points(point_range.clone())),
                (true, 2) => filtered!(|| ($make)().within_points(point_range.clone())),
                (true, 3) => filtered!(|| ($make)().containing_points(point_range.clone())),
                (true, 4) => filtered!(|| ($make)().starting_in_points(point_range.clone())),
                (true, 5) => filtered!(|| ($make)().ending_in_points(point_range.clone())),
                _ => unreachable!(),
            }
        };
    }
    match (recipe.postorder, recipe.reverse) {
        (false, false) => ranged!(|| pair.packed.preorder()),
        (false, true) => ranged!(|| pair.packed.preorder().rev()),
        (true, false) => ranged!(|| pair.packed.postorder()),
        (true, true) => ranged!(|| pair.packed.postorder().rev()),
    }
    coverage.scans += expected.len();
}

type MatchDescription = (PatternIx, Vec<(CaptureIx, (usize, usize))>);
fn describe_match(
    found: &tree_squatter::QueryMatch<'_, '_>,
    entry: &ForestEntry,
) -> MatchDescription {
    (
        found.pattern_index,
        found
            .captures()
            .iter()
            .map(|capture| (capture.index, entry.identity(capture.node)))
            .collect(),
    )
}

fn configure_packed(
    cursor: &mut QueryCursor,
    config: &CursorConfig,
    source: &str,
    points: bool,
    unlimited: bool,
) {
    cursor
        .set_byte_range(0..0)
        .set_point_range(Point::new(0, 0)..Point::new(0, 0))
        .set_containing_byte_range(0..0)
        .set_containing_point_range(Point::new(0, 0)..Point::new(0, 0));
    cursor.set_max_start_depth(config.depth);
    cursor.set_match_limit(if unlimited {
        65536
    } else {
        config.limit.unwrap_or(65536)
    });
    let coordinate = |byte| {
        if points {
            point(source, byte)
        } else {
            Point::new(0, byte)
        }
    };
    if let Some(range) = &config.byte_range {
        if config.point {
            cursor.set_point_range(coordinate(range.start)..coordinate(range.end));
        } else {
            cursor.set_byte_range(range.clone());
        }
    }
    if let Some(range) = &config.containing {
        if config.point {
            cursor.set_containing_point_range(coordinate(range.start)..coordinate(range.end));
        } else {
            cursor.set_containing_byte_range(range.clone());
        }
    }
}

fn configure_native(
    cursor: &mut tree_sitter::QueryCursor,
    config: &CursorConfig,
    source: &str,
    points: bool,
    unlimited: bool,
) {
    cursor
        .set_byte_range(0..0)
        .set_point_range(Point::new(0, 0)..Point::new(0, 0))
        .set_containing_byte_range(0..0)
        .set_containing_point_range(Point::new(0, 0)..Point::new(0, 0));
    cursor.set_max_start_depth(config.depth);
    cursor.set_match_limit(if unlimited {
        65536
    } else {
        config.limit.unwrap_or(65536)
    });
    if let Some(range) = &config.byte_range {
        if config.point && points {
            cursor.set_point_range(point(source, range.start)..point(source, range.end));
        } else {
            cursor.set_byte_range(range.clone());
        }
    }
    if let Some(range) = &config.containing {
        if config.point && points {
            cursor.set_containing_point_range(point(source, range.start)..point(source, range.end));
        } else {
            cursor.set_containing_byte_range(range.clone());
        }
    }
}

fn packed_matches(
    cursor: &mut QueryCursor,
    query: &Query,
    scope: QueryScope<'_>,
    entry: &ForestEntry,
    documents: &[Document],
    script: u8,
) -> Vec<MatchDescription> {
    let source = |node: Node<'_>| {
        let document = entry.roots[node.id().tree().ix()].specification.document;
        &documents[document].source.as_bytes()[node.byte_range()]
    };
    // Splitting bytes, rather than str slices, also exercises cuts inside UTF-8.
    let provider = |node: Node<'_>| {
        source(node).chunks(if script.is_multiple_of(2) {
            MAX_BYTES
        } else {
            1
        })
    };
    let mut results = Vec::new();
    let requested = Cell::new(false);
    let calls = Cell::new(0);
    let mut callback = |_: &QueryCursorState| {
        calls.set(calls.get() + 1);
        if script == 5 && calls.get() <= 4 {
            requested.set(true);
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = QueryCursorOptions::default().progress_callback(&mut callback);
    if script == 4 {
        let mut matches = cursor.matches_with_options(query, scope, provider, options);
        while let Some(found) = matches.next() {
            results.push(describe_match(found, entry));
            assert!(results.len() <= MAX_RESULTS);
        }
    } else {
        let mut execution = cursor.execute_with_options(query, scope, provider, options);
        loop {
            if let Some(found) = execution.next_match() {
                results.push(describe_match(&found, entry));
                assert!(results.len() <= MAX_RESULTS);
            } else if !requested.replace(false) {
                break;
            }
        }
        assert_eq!(execution.error(), None);
    }
    results.sort();
    results
}

fn query_contract(
    cursor: &mut QueryCursor,
    query: &Query,
    scope: QueryScope<'_>,
    entry: &ForestEntry,
    documents: &[Document],
    script: u8,
    config: &mut CursorConfig,
) {
    let provider = |node: Node<'_>| {
        let source = documents[entry.roots[node.id().tree().ix()].specification.document]
            .source
            .as_bytes();
        source[node.byte_range()].chunks(3)
    };
    match script % 8 {
        0 | 5 => {}
        1 => {
            let mut execution = cursor.execute(query, scope, provider);
            let _ = execution.next_match();
            assert_eq!(execution.error(), None);
        }
        2 => {
            let mut execution = cursor.execute(query, scope, provider);
            for index in 0..8 {
                let Some((found, capture)) = execution.next_capture() else {
                    break;
                };
                assert!(capture.ix() < found.captures().len());
                for capture in found.captures() {
                    assert!(capture.index.ix() < query.capture_names().len());
                    entry.identity(capture.node);
                }
                let before = describe_match(&found, entry);
                if index % 2 == 0 {
                    found.remove();
                }
                assert_eq!(describe_match(&found, entry), before);
            }
            assert_eq!(execution.error(), None);
        }
        3 => {
            let mut matches = cursor.matches(query, scope, provider);
            let _ = matches.next();
            if config.point {
                matches.set_point_range(Point::new(0, 0)..Point::new(0, 0));
            } else {
                matches.set_byte_range(0..0);
            }
            config.byte_range = None;
        }
        4 => {
            let mut matches = cursor.matches(query, scope, provider);
            if let Some(found) = matches.next() {
                let before = describe_match(found, entry);
                found.remove();
                assert_eq!(describe_match(found, entry), before);
            }
        }
        6 => {
            let mut captures = cursor.captures(query, scope, provider);
            if let Some((found, capture)) = captures.next() {
                assert!(capture.ix() < found.captures().len());
                let before = describe_match(found, entry);
                found.remove();
                assert_eq!(describe_match(found, entry), before);
            }
            if config.point {
                captures.set_point_range(Point::new(0, 0)..Point::new(0, 0));
            } else {
                captures.set_byte_range(0..0);
            }
            config.byte_range = None;
        }
        7 => {
            let mut execution = cursor.execute(query, scope, provider);
            let id = execution.next_match().map(|found| found.id());
            if let Some(id) = id {
                execution.remove_match(id);
            }
        }
        _ => unreachable!(),
    }
}

struct QueryRecipe<'a> {
    region: usize,
    config: &'a CursorConfig,
    script: u8,
    scope: u8,
}

fn run_query(
    entry: &ForestEntry,
    documents: &[Document],
    query: &QueryPair,
    cursors: &mut CursorPair,
    recipe: QueryRecipe<'_>,
    coverage: &mut Coverage,
) {
    let QueryRecipe {
        region: region_index,
        config,
        script,
        scope: scope_mode,
    } = recipe;
    let region = entry.forest.regions().nth(region_index).unwrap();
    let trees: Vec<_> = region.trees().collect();
    let first = trees[0];
    let source = &documents[entry.roots[first.root_node().id().tree().ix()]
        .specification
        .document]
        .source;
    let native_first = entry.roots[first.root_node().id().tree().ix()].node();
    let (scope, native_first) = match scope_mode % 4 {
        0 => (first.root_node().into(), native_first),
        1 => (first.into(), native_first),
        2 => (region.into(), native_first),
        3 => (
            first
                .root_node()
                .named_child(NamedChildIx(0))
                .unwrap_or(first.root_node())
                .into(),
            native_first.named_child(0).unwrap_or(native_first),
        ),
        _ => unreachable!(),
    };
    let tree_indices: Vec<_> = if scope_mode % 4 == 2 {
        trees
            .iter()
            .map(|tree| tree.root_node().id().tree().ix())
            .collect()
    } else {
        vec![first.root_node().id().tree().ix()]
    };
    let mut effective = config.clone();
    for cursor in &mut cursors.packed {
        configure_packed(cursor, config, source, entry.metadata.points, false);
        let mut updated = config.clone();
        query_contract(
            cursor,
            &query.packed,
            scope,
            entry,
            documents,
            script,
            &mut updated,
        );
        effective = updated;
    }
    let mut expected: Vec<MatchDescription> = Vec::new();
    for &tree in &tree_indices {
        let native = if scope_mode % 4 == 3 {
            native_first
        } else {
            entry.roots[tree].node()
        };
        let source = &documents[entry.roots[tree].specification.document].source;
        configure_native(
            &mut cursors.native,
            &effective,
            source,
            entry.metadata.points,
            true,
        );
        let mut matches = cursors
            .native
            .matches(&query.native, native, source.as_bytes());
        while let Some(found) = matches.next() {
            expected.push((
                PatternIx(found.pattern_index),
                found
                    .captures()
                    .iter()
                    .map(|capture| {
                        (
                            CaptureIx(capture.index),
                            entry.native_identity(tree, capture.node),
                        )
                    })
                    .collect(),
            ));
            assert!(expected.len() <= MAX_RESULTS);
        }
    }
    expected.sort();
    let mut expected_captures = BTreeSet::new();
    if effective.byte_range.is_some() || effective.containing.is_some() {
        for &tree in &tree_indices {
            let native = if scope_mode % 4 == 3 {
                native_first
            } else {
                entry.roots[tree].node()
            };
            let source = &documents[entry.roots[tree].specification.document].source;
            configure_native(
                &mut cursors.native,
                &effective,
                source,
                entry.metadata.points,
                true,
            );
            let mut captures = cursors
                .native
                .captures(&query.native, native, source.as_bytes());
            while let Some((found, index)) = captures.next() {
                let capture = found.captures()[*index];
                expected_captures.insert((
                    PatternIx(found.pattern_index),
                    CaptureIx(capture.index),
                    entry.native_identity(tree, capture.node),
                ));
            }
        }
    } else {
        for (pattern, captures) in &expected {
            for &(capture, identity) in captures {
                // The capture API need not emit zero-width snapshots at byte zero.
                if entry.end_bytes[identity.0][identity.1] != 0 {
                    expected_captures.insert((*pattern, capture, identity));
                }
            }
        }
    }
    for (mode, cursor) in cursors.packed.iter_mut().enumerate() {
        cursor.set_match_limit(65536);
        let actual = packed_matches(cursor, &query.packed, scope, entry, documents, script);
        assert_eq!(
            actual,
            expected,
            "query={}, general={}, scope={scope_mode}, config={effective:?}",
            query.source,
            mode == 0
        );
        let mut fresh = QueryCursor::new();
        fresh.set_optimized(mode != 0);
        configure_packed(&mut fresh, &effective, source, entry.metadata.points, true);
        assert_eq!(
            actual,
            packed_matches(&mut fresh, &query.packed, scope, entry, documents, 0)
        );
        for (_, captures) in &actual {
            assert!(
                captures
                    .iter()
                    .all(|(_, (tree, _))| tree_indices.contains(tree))
            );
            if let Some((_, (first, _))) = captures.first() {
                assert!(captures.iter().all(|(_, (tree, _))| tree == first));
            }
        }
        let mut emitted = BTreeSet::new();
        let provider = |node: Node<'_>| {
            documents[entry.roots[node.id().tree().ix()].specification.document]
                .source
                .as_bytes()[node.byte_range()]
            .chunks(3)
        };
        let mut execution = cursor.execute(&query.packed, scope, provider);
        let mut events = 0;
        while let Some((found, index)) = execution.next_capture() {
            events += 1;
            assert!(events <= MAX_RESULTS);
            assert!(index.ix() < found.captures().len());
            let capture = found.captures()[index.ix()];
            assert!(capture.index.ix() < query.packed.capture_names().len());
            emitted.insert((
                found.pattern_index,
                capture.index,
                entry.identity(capture.node),
            ));
        }
        assert_eq!(execution.error(), None);
        assert!(
            expected_captures.is_subset(&emitted),
            "missing completed capture: expected={expected_captures:?}, emitted={emitted:?}, config={effective:?}"
        );
        coverage.matches += actual.len();
    }
}

fn panic_message(error: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = error.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = error.downcast_ref::<&str>() {
        message.to_string()
    } else {
        "non-string panic".into()
    }
}

fn execute(case: &Case, coverage: &mut Coverage) -> Result<(), TestCaseError> {
    let mut world = World::new(case.documents.clone());
    let mut interpreted = 0;
    for (index, operation) in case.operations.iter().enumerate() {
        eprintln_if_verbose(format_args!("step {index}: {operation:?}"));
        let result = catch_unwind(AssertUnwindSafe(|| {
            interpreted += 1 + if let Operation::Explore(views) = operation {
                views.len()
            } else {
                0
            };
            assert!(interpreted <= MAX_STEPS, "runtime step budget exceeded");
            world.step(operation, coverage);
        }));
        if let Err(error) = result {
            let queries: Vec<_> = world
                .model
                .queries
                .iter()
                .map(|query| {
                    (
                        LANGUAGES[query.language].name,
                        QUERIES[query.source],
                        &query.patterns,
                        &query.captures,
                    )
                })
                .collect();
            return Err(TestCaseError::fail(format!(
                "step {index}: {operation:?}\n{}\nqueries: {queries:?}\nlanguages: {:?}\nmodel: {:#?}\nrepaired case: {case:#?}",
                panic_message(error),
                LANGUAGES
                    .iter()
                    .map(|language| language.name)
                    .collect::<Vec<_>>(),
                world.model
            )));
        }
    }
    for entry in &world.forests {
        entry.check(&world.model.documents);
        coverage.nodes += entry.packed_ids.len();
        coverage.slab_bytes += entry.forest.as_bytes().len();
    }
    world.forests.clear();
    for counter in &world.counters {
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
    Ok(())
}

fn position_strategy() -> BoxedStrategy<Position> {
    prop_oneof![
        Just(Position::Zero),
        Just(Position::Start),
        Just(Position::End),
        Just(Position::AfterEnd),
        (0u16..900).prop_map(Position::Raw)
    ]
    .boxed()
}

fn child_strategy() -> BoxedStrategy<Child> {
    prop_oneof![
        Just(Child::First),
        Just(Child::Last),
        Just(Child::PastLast),
        Just(Child::Large),
        (0u8..40).prop_map(Child::Raw)
    ]
    .boxed()
}

fn presence_strategy() -> BoxedStrategy<Presence> {
    prop_oneof![
        Just(Presence::Default),
        Just(Presence::None),
        Just(Presence::All),
        any::<u8>().prop_map(Presence::Selected)
    ]
    .boxed()
}

fn options_strategy() -> BoxedStrategy<Options> {
    (any::<bool>(), any::<bool>(), presence_strategy())
        .prop_map(|(points, compact, presence)| Options {
            points,
            compact,
            presence,
        })
        .boxed()
}

fn scan_strategy() -> BoxedStrategy<ScanRecipe> {
    (
        any::<bool>(),
        any::<bool>(),
        0u8..6,
        position_strategy(),
        position_strategy(),
        0u8..6,
        0u8..12,
        any::<bool>(),
    )
        .prop_map(
            |(postorder, reverse, relation, start, end, filter, consume, points)| ScanRecipe {
                postorder,
                reverse,
                relation,
                start,
                end,
                filter,
                consume,
                points,
            },
        )
        .boxed()
}

fn navigation_strategy() -> BoxedStrategy<Navigation> {
    prop_oneof![
        Just(Navigation::Parent),
        child_strategy().prop_map(Navigation::Child),
        child_strategy().prop_map(Navigation::NamedChild),
        Just(Navigation::Next),
        Just(Navigation::Previous),
        Just(Navigation::NextNamed),
        Just(Navigation::PreviousNamed),
        (0usize..7).prop_map(Navigation::Field),
        position_strategy().prop_map(Navigation::FirstByte),
        position_strategy().prop_map(Navigation::FirstNamedByte),
        (
            position_strategy(),
            position_strategy(),
            any::<bool>(),
            any::<bool>()
        )
            .prop_map(|(start, end, named, points)| Navigation::Descendant(
                start, end, named, points
            )),
    ]
    .boxed()
}

fn view_strategy() -> BoxedStrategy<ViewOperation> {
    prop_oneof![
        3 => (0usize..48).prop_map(ViewOperation::Read),
        8 => (0usize..48, navigation_strategy()).prop_map(|(node, navigation)| ViewOperation::Navigate { node, navigation }),
        1 => (0usize..48).prop_map(ViewOperation::DropNode),
        1 => (0usize..48).prop_map(ViewOperation::NewCursor),
        2 => (0usize..48).prop_map(ViewOperation::CloneCursor),
        1 => (0usize..48).prop_map(ViewOperation::DropCursor),
        2 => (0usize..48).prop_map(ViewOperation::ReadCursor),
        7 => (0usize..48, 0u8..7, position_strategy()).prop_map(|(cursor, movement, position)| ViewOperation::Move { cursor, movement, position }),
        2 => (0usize..48, 0usize..48).prop_map(|(cursor, node)| ViewOperation::Reset { cursor, node }),
        2 => (0usize..48, 0usize..48).prop_map(|(cursor, other)| ViewOperation::ResetTo { cursor, other }),
        6 => (0usize..48, scan_strategy()).prop_map(|(node, recipe)| ViewOperation::Scan { node, recipe }),
    ].boxed()
}

fn config_strategy() -> BoxedStrategy<CursorConfig> {
    let range = || (0usize..800, 0usize..800).prop_map(|(start, end)| start..end);
    (
        proptest::option::of(range()),
        proptest::option::of(range()),
        proptest::option::of(0u32..6),
        proptest::option::of(1u32..8),
        any::<bool>(),
    )
        .prop_map(
            |(byte_range, containing, depth, limit, point)| CursorConfig {
                byte_range,
                containing,
                depth,
                limit,
                point,
            },
        )
        .boxed()
}

fn operation_strategy() -> BoxedStrategy<Operation> {
    let documents = prop_oneof![
        (0usize..5, 0usize..5).prop_map(|(language, source)| Operation::Add { language, source }),
        (0usize..48, 0u16..900, 0u16..900, 0usize..INSERTIONS.len()).prop_map(
            |(document, start, end, insertion)| Operation::Splice {
                document,
                start,
                end,
                insertion
            }
        ),
    ];
    let parsers = prop_oneof![
        3 => (0usize..5).prop_map(|language| Operation::NewParser { language }),
        1 => (0usize..48).prop_map(Operation::ParserScratch),
        1 => (0usize..48).prop_map(Operation::ResetParser),
        1 => (0usize..48).prop_map(Operation::DropParser),
        2 => (0usize..48).prop_map(Operation::InvalidParse),
        1 => (0usize..48).prop_map(Operation::CancelParse),
    ];
    let packers = prop_oneof![
        Just(Operation::NewPacker),
        (0usize..48).prop_map(Operation::PackerScratch),
        (0usize..48).prop_map(Operation::DropPacker),
        (0usize..48).prop_map(Operation::InvalidRegion)
    ];
    let storage = prop_oneof![
        5 => (0usize..48, 0usize..7).prop_map(|(forest, mode)| Operation::Copy { forest, mode: [CopyMode::Compact, CopyMode::Detach, CopyMode::Load, CopyMode::SafetyChecked, CopyMode::Retain, CopyMode::TrustedLoad, CopyMode::TrustedRetain][mode] }),
        2 => (0usize..48).prop_map(Operation::Compact), 2 => (0usize..48).prop_map(Operation::DropForest),
        2 => (0usize..48, any::<bool>()).prop_map(|(forest, points)| Operation::Save { forest, side: if points { Side::Points } else { Side::Presence } }),
        3 => (0usize..48, 0usize..48, any::<bool>()).prop_map(|(forest, saved, retained)| Operation::Restore { forest, saved, retained }),
        1 => (0usize..48).prop_map(Operation::DropSaved),
        2 => (0usize..48, any::<bool>()).prop_map(|(forest, points)| Operation::DropSide { forest, side: if points { Side::Points } else { Side::Presence } }),
        3 => (0usize..48, presence_strategy()).prop_map(|(forest, presence)| Operation::BuildPresence { forest, presence }),
        1 => (0usize..48).prop_map(Operation::CancelPresence), 1 => (0usize..48).prop_map(Operation::InvalidSide),
    ];
    let queries = prop_oneof![
        5 => (0usize..5, 0usize..QUERIES.len()).prop_map(|(language, source)| Operation::NewQuery { language, source }),
        1 => (0usize..48).prop_map(Operation::CloneQuery), 1 => (0usize..48).prop_map(Operation::DropQuery),
        2 => (0usize..48, 0usize..8).prop_map(|(query, pattern)| Operation::DisablePattern { query, pattern }),
        2 => (0usize..48, 0usize..8).prop_map(|(query, capture)| Operation::DisableCapture { query, capture }),
        1 => (0usize..5).prop_map(Operation::InvalidQuery), 4 => Just(Operation::NewQueryCursor),
        1 => (0usize..48).prop_map(Operation::DropQueryCursor),
        3 => (0usize..48, config_strategy()).prop_map(|(cursor, config)| Operation::Configure { cursor, config }),
        8 => (0usize..48, 0usize..48, 0usize..48, 0usize..48, 0u8..8, 0u8..4).prop_map(|(forest, region, query, cursor, script, scope)| Operation::Execute { forest, region, query, cursor, script, scope }),
        2 => (0usize..48, 0usize..48, 0usize..48).prop_map(|(forest, query, cursor)| Operation::WrongGrammar { forest, query, cursor }),
    ];
    prop_oneof![
        3 => documents, 4 => parsers, 2 => packers,
        8 => (0usize..48, 0usize..48, options_strategy(), prop_oneof![Just(0u8), Just(1u8), Just(3u8), Just(8u8)]).prop_map(|(parser, document, options, chunk)| Operation::Parse { parser, document, options, chunk }),
        6 => (0usize..48, proptest::collection::vec(proptest::collection::vec((0usize..48, 0u8..5), 1..4), 0..5), options_strategy(), any::<bool>()).prop_map(|(packer, regions, options, mixed)| Operation::Pack { packer, regions, options, mixed }),
        8 => storage, 8 => queries,
        12 => proptest::collection::vec(view_strategy(), 1..33).prop_map(Operation::Explore),
    ].boxed()
}

fn json_strategy() -> BoxedStrategy<String> {
    let leaf = prop_oneof![
        Just("null".to_string()),
        any::<bool>().prop_map(|value| value.to_string()),
        (-100i32..100).prop_map(|value| value.to_string()),
        prop_oneof![Just("\"π😀\"".to_string()), Just("\"x\"".to_string())]
    ];
    prop_oneof![
        6 => leaf.prop_recursive(4, 64, 4, |inner| prop_oneof![proptest::collection::vec(inner.clone(), 0..5).prop_map(|values| format!("[{}]", values.join(","))), proptest::collection::vec(inner, 0..5).prop_map(|values| format!("{{{}}}", values.into_iter().enumerate().map(|(index, value)| format!("\"key{index}\":{value}")).collect::<Vec<_>>().join(",")))]),
        2 => prop_oneof![Just(31usize), Just(32), Just(33), Just(63), Just(64), Just(65)].prop_map(|width| format!("[{}0]", "[1,2],".repeat(width))),
        1 => prop_oneof![Just(255usize), Just(256), Just(257)].prop_map(|length| format!("[\r\n\"{}π😀\",\n0]", "x".repeat(length))),
        1 => (1usize..66).prop_map(|depth| format!("{}0{}", "[".repeat(depth), "]".repeat(depth))),
    ].boxed()
}

fn case_strategy() -> BoxedStrategy<Case> {
    (
        proptest::collection::vec(json_strategy(), 1..4),
        proptest::collection::vec(operation_strategy(), 0..20),
        0u8..3,
        0usize..QUERIES.len(),
        0u8..8,
        (
            prop_oneof![1 => Just(false), 4 => Just(true)],
            prop_oneof![1 => Just(false), 4 => Just(true)],
            prop_oneof![1 => Just(false), 4 => Just(true)],
            prop_oneof![1 => Just(false), 4 => Just(true)],
        ),
    )
        .prop_map(
            |(sources, mut operations, branch, query, script, producers)| {
                let mut documents: Vec<_> = (0..LANGUAGES.len())
                    .map(|language| fixture(language, 0))
                    .collect();
                documents.extend(sources.into_iter().map(|source| Document {
                    language: 0,
                    source: source.into(),
                    valid: true,
                }));
                let mut prefix = Vec::new();
                if branch != 0 {
                    if producers.0 {
                        prefix.push(Operation::Pack {
                            packer: 0,
                            regions: vec![vec![(5, 0)]],
                            options: Options::default(),
                            mixed: false,
                        });
                    }
                    if producers.1 {
                        prefix.push(Operation::NewQuery {
                            language: 0,
                            source: query,
                        });
                    }
                    if producers.2 {
                        prefix.push(Operation::NewQueryCursor);
                    }
                    if producers.3 {
                        prefix.push(Operation::Execute {
                            forest: 1,
                            region: 0,
                            query: 0,
                            cursor: 0,
                            script,
                            scope: branch,
                        });
                    }
                }
                prefix.append(&mut operations);
                Case {
                    documents,
                    operations: prefix,
                }
            },
        )
        .boxed()
}

fn record(total: &mut Coverage, coverage: &Coverage) {
    total.generated += coverage.generated;
    total.admitted += coverage.admitted;
    total.executed += coverage.executed;
    total.absent += coverage.absent;
    total.budget += coverage.budget;
    total.views += coverage.views;
    for index in 0..2 {
        total.navigation[index] += coverage.navigation[index];
    }
    for index in 0..4 {
        total.parses[index] += coverage.parses[index];
    }
    for index in 0..7 {
        total.copies[index] += coverage.copies[index];
    }
    total.direct_failures += coverage.direct_failures;
    total.mixed += coverage.mixed;
    total.nodes += coverage.nodes;
    total.slab_bytes += coverage.slab_bytes;
    total.scans += coverage.scans;
    total.matches += coverage.matches;
    total.failures += coverage.failures;
}

fn pool_config() -> Config {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 128;
    }
    config.max_shrink_iters = 4096;
    config.source_file = Some(file!());
    config.test_name = Some(concat!(module_path!(), "::bisim"));
    config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/bisim.proptest-regressions"
    ))));
    config
}

fn pool_worker(config: Config) -> (Coverage, Result<(), String>) {
    let mut runner = TestRunner::new(config);
    let total = RefCell::new(Coverage::default());
    let shrinking = Cell::new(false);
    let result = runner.run(&case_strategy(), |raw| {
        let mut coverage = Coverage::default();
        let case = repair(&raw, &mut coverage);
        let result = catch_unwind(AssertUnwindSafe(|| execute(&case, &mut coverage)))
            .unwrap_or_else(|error| {
                Err(TestCaseError::fail(format!(
                    "{}\nrepaired case: {case:#?}",
                    panic_message(error)
                )))
            });
        if result.is_err() {
            shrinking.set(true);
        }
        if !shrinking.get() {
            record(&mut total.borrow_mut(), &coverage);
        }
        result
    });
    (
        total.into_inner(),
        result.map_err(|error| error.to_string()),
    )
}

#[test]
fn bisim() {
    let (total, result) = pool_worker(pool_config());
    eprintln!("pool coverage (excluding shrinking): {total:?}");
    if let Err(error) = result {
        panic!("{error}");
    }
}

fn parse_jobs(arguments: impl IntoIterator<Item = String>) -> Result<usize, String> {
    let mut jobs = std::thread::available_parallelism().map_or(1, usize::from);
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let value = if argument == "-j" {
            arguments.next().ok_or("-j requires a worker count")?
        } else if let Some(value) = argument.strip_prefix("-j") {
            value.to_string()
        } else {
            return Err(format!("unknown argument: {argument}"));
        };
        jobs = value
            .parse::<usize>()
            .ok()
            .filter(|&jobs| jobs > 0)
            .ok_or("-j requires a positive worker count")?;
    }
    Ok(jobs)
}

fn worker_config(config: &Config, worker: usize, jobs: usize) -> Config {
    let mut config = config.clone();
    config.cases =
        (config.cases as usize / jobs + usize::from(worker < config.cases as usize % jobs)) as u32;
    if let RngSeed::Fixed(seed) = config.rng_seed {
        config.rng_seed = RngSeed::Fixed(seed.wrapping_add(worker as u64));
    }
    config
}

#[cfg(not(test))]
pub(crate) fn run_cli() -> std::process::ExitCode {
    use proptest::strategy::ValueTree;

    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.iter().any(|argument| argument == "--help") {
        println!(
            "bisim [-j N]\n\nRun independent case pools concurrently (default: available CPUs).\nPROPTEST_CASES sets the total case count; PROPTEST_RNG_SEED sets the base seed."
        );
        return std::process::ExitCode::SUCCESS;
    }
    let jobs = match parse_jobs(arguments) {
        Ok(jobs) => jobs,
        Err(error) => {
            eprintln!("{error}");
            return std::process::ExitCode::from(2);
        }
    };
    run_fixed_cases();
    let mut config = pool_config();
    if config.fork() {
        eprintln!("fork/timeout options require the cargo test entry point");
        return std::process::ExitCode::from(2);
    }
    if config.rng_seed == RngSeed::Random {
        config.rng_seed = RngSeed::Fixed(
            any::<u64>()
                .new_tree(&mut TestRunner::new(config.clone()))
                .unwrap()
                .current(),
        );
    }
    let jobs = jobs.min(config.cases.max(1) as usize);
    eprintln!(
        "pool cases: {}; workers: {jobs}; base seed: {}",
        config.cases, config.rng_seed
    );
    let start = std::time::Instant::now();
    let mut total = Coverage::default();
    let mut failed = false;
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..jobs)
            .map(|worker| {
                let config = worker_config(&config, worker, jobs);
                std::thread::Builder::new()
                    .name(format!("bisim-worker-{worker}"))
                    .spawn_scoped(scope, move || {
                        let cases = config.cases;
                        let seed = config.rng_seed;
                        let (coverage, result) = pool_worker(config);
                        eprintln!(
                            "worker {worker}: target {cases} cases; seed {seed}; {}",
                            if result.is_ok() { "passed" } else { "FAILED" }
                        );
                        (coverage, result)
                    })
                    .expect("spawn pool worker")
            })
            .collect();
        for worker in workers {
            match worker.join() {
                Ok((coverage, result)) => {
                    record(&mut total, &coverage);
                    if let Err(error) = result {
                        failed = true;
                        eprintln!("{error}");
                    }
                }
                Err(error) => {
                    failed = true;
                    eprintln!("worker panicked: {}", panic_message(error));
                }
            }
        }
    });
    eprintln!("pool coverage (excluding shrinking): {total:?}");
    eprintln!("elapsed: {:.2?}", start.elapsed());
    if failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

#[cfg_attr(test, test)]
fn concurrent_runner_configuration() {
    assert_eq!(parse_jobs(["-j".into(), "3".into()]).unwrap(), 3);
    assert_eq!(parse_jobs(["-j3".into()]).unwrap(), 3);
    for arguments in [vec!["-j"], vec!["-j0"], vec!["-jno"], vec!["--unknown"]] {
        assert!(parse_jobs(arguments.into_iter().map(String::from)).is_err());
    }
    let config = Config {
        cases: 10,
        rng_seed: RngSeed::Fixed(u64::MAX),
        ..pool_config()
    };
    let workers: Vec<_> = (0..4)
        .map(|worker| worker_config(&config, worker, 4))
        .collect();
    assert_eq!(
        workers
            .iter()
            .map(|config| config.cases)
            .collect::<Vec<_>>(),
        [3, 3, 2, 2]
    );
    assert_eq!(
        workers
            .iter()
            .map(|config| config.rng_seed)
            .collect::<Vec<_>>(),
        [
            RngSeed::Fixed(u64::MAX),
            RngSeed::Fixed(0),
            RngSeed::Fixed(1),
            RngSeed::Fixed(2)
        ]
    );
    assert_eq!(worker_config(&config, 0, 1), config);
}

#[cfg(not(test))]
fn run_fixed_cases() {
    let tests: &[(&str, fn())] = &[
        (
            "concurrent_runner_configuration",
            concurrent_runner_configuration,
        ),
        ("mixed_forest_lifecycles", mixed_forest_lifecycles),
        (
            "supertype_scans_from_descendants",
            supertype_scans_from_descendants,
        ),
        (
            "disabled_parent_capture_with_repeated_children",
            disabled_parent_capture_with_repeated_children,
        ),
        ("parser_and_packer_reuse", parser_and_packer_reuse),
        ("repair_pool_boundaries", repair_pool_boundaries),
        (
            "query_changes_failures_and_reuse",
            query_changes_failures_and_reuse,
        ),
        (
            "cancellation_and_pause_resume_are_exercised",
            cancellation_and_pause_resume_are_exercised,
        ),
        (
            "query_range_narrowing_and_projected_points",
            query_range_narrowing_and_projected_points,
        ),
        (
            "empty_forests_and_incompatible_sidecars",
            empty_forests_and_incompatible_sidecars,
        ),
    ];
    for &(name, test) in tests {
        test();
        eprintln!("{name}: passed");
    }
}

fn fixed_case(operations: Vec<Operation>) -> Case {
    Case {
        documents: (0..LANGUAGES.len())
            .map(|language| fixture(language, 0))
            .collect(),
        operations,
    }
}

fn check_fixed(case: Case) {
    let mut coverage = Coverage::default();
    let repaired = repair(&case, &mut coverage);
    assert_eq!(
        repaired.operations.len(),
        case.operations.len(),
        "fixed operation was repaired away: {case:#?}"
    );
    execute(&repaired, &mut coverage).unwrap();
}

fn fixed_views() -> Vec<ViewOperation> {
    let mut operations = Vec::new();
    for node in 0..8 {
        operations.push(ViewOperation::Read(node));
        operations.push(ViewOperation::Navigate {
            node,
            navigation: Navigation::Child(Child::PastLast),
        });
        operations.push(ViewOperation::Move {
            cursor: node,
            movement: 0,
            position: Position::Zero,
        });
        operations.push(ViewOperation::CloneCursor(node));
        operations.push(ViewOperation::Reset {
            cursor: node,
            node: (node + 1) % 8,
        });
        operations.push(ViewOperation::ResetTo {
            cursor: node,
            other: node + 10,
        });
        for postorder in [false, true] {
            operations.push(ViewOperation::Scan {
                node,
                recipe: ScanRecipe {
                    postorder,
                    reverse: postorder,
                    relation: 1,
                    start: Position::Start,
                    end: Position::AfterEnd,
                    filter: 0,
                    consume: 2,
                    points: postorder,
                },
            });
        }
    }
    operations
}

#[cfg_attr(test, test)]
fn disabled_parent_capture_with_repeated_children() {
    check_fixed(fixed_case(vec![
        Operation::Pack {
            packer: 0,
            regions: vec![vec![(4, 0)]],
            options: Options::default(),
            mixed: false,
        },
        Operation::NewQuery {
            language: 4,
            source: 7,
        },
        Operation::DisableCapture {
            query: 0,
            capture: 1,
        },
        Operation::NewQueryCursor,
        Operation::Execute {
            forest: 1,
            region: 0,
            query: 0,
            cursor: 0,
            script: 0,
            scope: 0,
        },
    ]));
}

#[cfg_attr(test, test)]
fn supertype_scans_from_descendants() {
    let mut views = vec![
        ViewOperation::Navigate {
            node: 2,
            navigation: Navigation::FirstNamedByte(Position::Zero),
        },
        ViewOperation::ReadCursor(0),
    ];
    for node in [0, 2, 3, 6, 7, 10] {
        for consume in 0..16 {
            views.push(ViewOperation::Scan {
                node,
                recipe: ScanRecipe {
                    postorder: false,
                    reverse: false,
                    relation: 0,
                    start: Position::Zero,
                    end: Position::Zero,
                    filter: 5,
                    consume,
                    points: false,
                },
            });
        }
    }
    check_fixed(fixed_case(vec![
        Operation::Pack {
            packer: 0,
            regions: vec![vec![(3, 1), (3, 0)], vec![(0, 0), (0, 0)]],
            options: Options {
                points: false,
                ..Options::default()
            },
            mixed: false,
        },
        Operation::Explore(views),
    ]));
}

#[cfg_attr(test, test)]
fn mixed_forest_lifecycles() {
    let mut operations = vec![
        Operation::Pack {
            packer: 0,
            regions: vec![
                vec![(0, 0)],
                vec![(1, 0)],
                vec![(2, 0)],
                vec![(0, 0), (0, 1)],
            ],
            options: Options {
                presence: Presence::All,
                ..Default::default()
            },
            mixed: true,
        },
        Operation::Explore(fixed_views()[..32].to_vec()),
        Operation::Explore(fixed_views()[32..].to_vec()),
        Operation::Save {
            forest: 1,
            side: Side::Points,
        },
        Operation::Save {
            forest: 1,
            side: Side::Presence,
        },
        Operation::Copy {
            forest: 1,
            mode: CopyMode::Retain,
        },
        Operation::DropForest(1),
        Operation::Restore {
            forest: 1,
            saved: 0,
            retained: true,
        },
        Operation::Restore {
            forest: 1,
            saved: 1,
            retained: true,
        },
        Operation::Copy {
            forest: 1,
            mode: CopyMode::Detach,
        },
        Operation::Compact(1),
        Operation::DropForest(1),
        Operation::DropSide {
            forest: 1,
            side: Side::Points,
        },
        Operation::Restore {
            forest: 1,
            saved: 0,
            retained: false,
        },
        Operation::BuildPresence {
            forest: 1,
            presence: Presence::Selected(5),
        },
        Operation::Explore(vec![ViewOperation::Read(1)]),
        Operation::Copy {
            forest: 1,
            mode: CopyMode::Compact,
        },
    ];
    for language in [0, 1, 2] {
        operations.push(Operation::NewQuery {
            language,
            source: 1,
        });
        if language == 0 {
            operations.push(Operation::NewQueryCursor);
        }
        operations.push(Operation::Execute {
            forest: 1,
            region: language,
            query: language,
            cursor: 0,
            script: 2,
            scope: 2,
        });
    }
    check_fixed(fixed_case(operations));
}

#[cfg_attr(test, test)]
fn parser_and_packer_reuse() {
    for language in 0..LANGUAGES.len() {
        let mut operations = vec![Operation::NewParser { language }, Operation::NewPacker];
        for (index, _) in LANGUAGES[language].sources.iter().enumerate() {
            operations.push(Operation::Add {
                language,
                source: index,
            });
            operations.push(Operation::Parse {
                parser: 1,
                document: index + 1,
                options: Options {
                    points: index % 2 == 0,
                    compact: index % 2 == 0,
                    presence: Presence::All,
                },
                chunk: [0, 1, 3][index % 3],
            });
            operations.push(Operation::ParserScratch(1));
            operations.push(Operation::ResetParser(1));
        }
        if LANGUAGES[language].native.abi_version() >= 15 {
            operations.push(Operation::InvalidParse(1));
        }
        operations.push(Operation::CancelParse(1));
        operations.extend([
            Operation::DropParser(1),
            Operation::PackerScratch(1),
            Operation::InvalidRegion(1),
            Operation::DropPacker(1),
            Operation::Explore(vec![ViewOperation::Read(2)]),
        ]);
        check_fixed(fixed_case(operations));
    }
}

#[cfg_attr(test, test)]
fn repair_pool_boundaries() {
    let raw = fixed_case(vec![
        Operation::DropForest(0),
        Operation::Copy {
            forest: 10,
            mode: CopyMode::Detach,
        },
        Operation::Explore(vec![ViewOperation::Read(0)]),
    ]);
    let repaired = repair(&raw, &mut Coverage::default());
    assert_eq!(repaired.operations.len(), 1);
    execute(&repaired, &mut Coverage::default()).unwrap();
    check_fixed(fixed_case(vec![
        Operation::Copy {
            forest: 0,
            mode: CopyMode::Detach,
        },
        Operation::DropForest(0),
        Operation::Explore(vec![
            ViewOperation::Navigate {
                node: 0,
                navigation: Navigation::Child(Child::Large),
            },
            ViewOperation::Read(2),
            ViewOperation::DropNode(0),
            ViewOperation::Read(1),
        ]),
        Operation::Copy {
            forest: 9,
            mode: CopyMode::Load,
        },
    ]));
    let raw = fixed_case(vec![
        Operation::DropParser(0),
        Operation::Parse {
            parser: 0,
            document: 0,
            options: Options::default(),
            chunk: 0,
        },
    ]);
    assert_eq!(repair(&raw, &mut Coverage::default()).operations.len(), 1);
}

#[cfg_attr(test, test)]
fn query_changes_failures_and_reuse() {
    check_fixed(fixed_case(vec![
        Operation::NewQuery {
            language: 0,
            source: 8,
        },
        Operation::CloneQuery(0),
        Operation::NewQueryCursor,
        Operation::DisablePattern {
            query: 0,
            pattern: 0,
        },
        Operation::Execute {
            forest: 0,
            region: 0,
            query: 0,
            cursor: 0,
            script: 1,
            scope: 0,
        },
        Operation::DisableCapture {
            query: 1,
            capture: 0,
        },
        Operation::Execute {
            forest: 0,
            region: 0,
            query: 1,
            cursor: 0,
            script: 3,
            scope: 3,
        },
        Operation::DisablePattern {
            query: 0,
            pattern: 1,
        },
        Operation::Execute {
            forest: 0,
            region: 0,
            query: 0,
            cursor: 0,
            script: 4,
            scope: 1,
        },
        Operation::DropQuery(0),
        Operation::NewQuery {
            language: 1,
            source: 1,
        },
        Operation::WrongGrammar {
            forest: 0,
            query: 1,
            cursor: 0,
        },
        Operation::Execute {
            forest: 0,
            region: 0,
            query: 0,
            cursor: 0,
            script: 6,
            scope: 2,
        },
        Operation::Configure {
            cursor: 0,
            config: CursorConfig {
                byte_range: Some(1..12),
                containing: Some(0..12),
                depth: Some(2),
                limit: Some(1),
                point: true,
            },
        },
        Operation::Execute {
            forest: 0,
            region: 0,
            query: 0,
            cursor: 0,
            script: 7,
            scope: 0,
        },
        Operation::Execute {
            forest: 0,
            region: 0,
            query: 0,
            cursor: 0,
            script: 5,
            scope: 1,
        },
        Operation::Configure {
            cursor: 0,
            config: CursorConfig {
                byte_range: Some(4..4),
                ..Default::default()
            },
        },
        Operation::Execute {
            forest: 0,
            region: 0,
            query: 0,
            cursor: 0,
            script: 0,
            scope: 0,
        },
        Operation::DropQueryCursor(0),
        Operation::InvalidQuery(0),
    ]));
}

#[cfg_attr(test, test)]
fn cancellation_and_pause_resume_are_exercised() {
    let source = format!("[{}]", "[1,2,3,4],".repeat(600).trim_end_matches(','));
    assert!(source.len() <= MAX_BYTES);
    let native = support::parse_native(&LANGUAGES[0].native, &source);
    let mut compatible = tree_squatter::Parser::new();
    compatible.set_language(&LANGUAGES[0].packed).unwrap();
    let mut parse_calls = 0;
    let mut cancel_parse = |_: &dyn ParseStateLike| {
        parse_calls += 1;
        ControlFlow::Break(())
    };
    let result = compatible.parse_with_options(
        &mut |byte, _| &source.as_bytes()[byte..],
        tree_squatter::ParseOptions::new()
            .progress_callback(&mut cancel_parse)
            .into(),
    );
    assert_eq!(result.unwrap_err(), tree_squatter::ParserError::Canceled);
    assert!(parse_calls > 0);
    compatible.reset();
    let forest = compatible.parse(&source).unwrap();
    support::assert_same_tree(
        &forest,
        &Forest::pack(&LANGUAGES[0].packed, &native).unwrap(),
    );
    let pack_calls = Cell::new(0);
    let cancel_pack = || {
        pack_calls.set(pack_calls.get() + 1);
        ControlFlow::Break(())
    };
    let options = PackOptions {
        symbol_presence: &|_| true,
        cancellation_callback: Some(&cancel_pack),
        ..Default::default()
    };
    let mut packer = Packer::new().unwrap();
    assert_eq!(
        packer
            .pack_with_options(&LANGUAGES[0].packed, &native, options)
            .unwrap_err(),
        Error::Canceled
    );
    assert!(pack_calls.get() > 0);
    support::assert_same_tree(
        &packer.pack(&LANGUAGES[0].packed, &native).unwrap(),
        &forest,
    );
    let query = Query::new(&LANGUAGES[0].packed, "(number) @number").unwrap();
    for optimized in [false, true] {
        let mut cursor = QueryCursor::new();
        cursor.set_optimized(optimized);
        let expected: Vec<_> = support::query_results(
            &mut cursor,
            &query,
            forest.root_node(),
            source.as_bytes(),
            false,
        );
        let calls = Cell::new(0);
        let requested = Cell::new(false);
        let mut stop = |_: &QueryCursorState| {
            calls.set(calls.get() + 1);
            if calls.get() <= 4 {
                requested.set(true);
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut execution = cursor.execute_with_options(
            &query,
            forest.root_node(),
            source.as_bytes(),
            QueryCursorOptions::default().progress_callback(&mut stop),
        );
        let mut actual = Vec::new();
        let mut stops = 0;
        loop {
            if let Some(found) = execution.next_match() {
                actual.push(support::query_snapshot(&found, None));
            } else if requested.replace(false) {
                stops += 1;
            } else {
                break;
            }
        }
        assert_eq!(execution.error(), None);
        assert!(calls.get() > 4);
        assert!(stops > 0);
        assert_eq!(actual, expected);
        drop(execution);
        cursor.set_match_limit(1);
        let explosive = Query::new(
            &LANGUAGES[0].packed,
            "(array (number)* @left (number)* @right)",
        )
        .unwrap();
        let mut execution = cursor.execute(&explosive, forest.root_node(), source.as_bytes());
        while execution.next_match().is_some() {}
        drop(execution);
        assert!(cursor.did_exceed_match_limit());
        cursor.set_match_limit(65536);
        assert_eq!(
            support::query_results(
                &mut cursor,
                &query,
                forest.root_node(),
                source.as_bytes(),
                false
            ),
            expected
        );
        assert!(!cursor.did_exceed_match_limit());
    }
    let mut direct = TreeFellerParser::new(&LANGUAGES[1].packed).unwrap();
    let mut calls = 0;
    let mut cancel = |_: &dyn ParseStateLike| {
        calls += 1;
        ControlFlow::Break(())
    };
    let source = LANGUAGES[1].sources[0].as_bytes();
    let forest = direct
        .parse_with_options(
            &mut |byte, _| &source[byte..(byte + 1).min(source.len())],
            tree_squatter::ParseOptions::new()
                .progress_callback(&mut cancel)
                .into(),
        )
        .unwrap();
    assert_eq!(calls, 0);
    forest.validate().unwrap();
    assert_eq!(
        direct
            .parse_with_options(
                &mut |byte, _| &source[byte..],
                PackedParseOptions {
                    pack: options,
                    ..Default::default()
                }
            )
            .unwrap_err()
            .code,
        Error::Canceled
    );
    direct.parse(source).unwrap().validate().unwrap();
}

#[cfg_attr(test, test)]
fn query_range_narrowing_and_projected_points() {
    let source = "[1,\n2]";
    let native = support::parse_native(&LANGUAGES[0].native, source);
    let mut forest = Forest::pack(&LANGUAGES[0].packed, &native).unwrap();
    let query = Query::new(&LANGUAGES[0].packed, "(number) @number").unwrap();
    for points in [true, false] {
        if !points {
            forest.drop_point_data();
        }
        for optimized in [false, true] {
            let mut cursor = QueryCursor::new();
            cursor.set_optimized(optimized);
            if !points {
                cursor.set_point_range(Point::new(1, 0)..Point::new(2, 0));
                assert!(
                    support::query_results(
                        &mut cursor,
                        &query,
                        forest.root_node(),
                        source.as_bytes(),
                        false
                    )
                    .is_empty()
                );
                cursor.set_point_range(Point::new(0, 0)..Point::new(1, 0));
                assert_eq!(
                    support::query_results(
                        &mut cursor,
                        &query,
                        forest.root_node(),
                        source.as_bytes(),
                        false
                    )
                    .len(),
                    2
                );
            }
            cursor.set_point_range(Point::new(0, 0)..Point::new(0, 0));
            cursor.set_byte_range(1..2);
            let expected = support::query_results(
                &mut cursor,
                &query,
                forest.root_node(),
                source.as_bytes(),
                false,
            );
            cursor.set_byte_range(Range { start: 5, end: 2 });
            assert_eq!(
                support::query_results(
                    &mut cursor,
                    &query,
                    forest.root_node(),
                    source.as_bytes(),
                    false
                ),
                expected
            );
            if usize::BITS > 32 {
                let wrap = (u32::MAX as usize).wrapping_add(1);
                cursor.set_byte_range(wrap + 1..wrap + 2);
                assert_eq!(
                    support::query_results(
                        &mut cursor,
                        &query,
                        forest.root_node(),
                        source.as_bytes(),
                        false
                    ),
                    expected
                );
                assert_eq!(
                    forest
                        .root_node()
                        .descendant_for_byte_range(wrap + 1, wrap + 2)
                        .unwrap()
                        .byte_range(),
                    1..2
                );
                cursor.set_byte_range(0..0);
                let end = Point::new(0, 2);
                cursor.set_point_range(
                    Point::new(wrap, wrap + 1)..Point::new(wrap + end.row, wrap + end.column),
                );
                assert_eq!(
                    support::query_results(
                        &mut cursor,
                        &query,
                        forest.root_node(),
                        source.as_bytes(),
                        false
                    ),
                    expected
                );
                cursor.set_point_range(Point::new(0, 5)..Point::new(0, 2));
                assert_eq!(
                    support::query_results(
                        &mut cursor,
                        &query,
                        forest.root_node(),
                        source.as_bytes(),
                        false
                    ),
                    expected
                );
            }
        }
    }
}

#[cfg_attr(test, test)]
fn empty_forests_and_incompatible_sidecars() {
    check_fixed(fixed_case(vec![
        Operation::Pack {
            packer: 0,
            regions: Vec::new(),
            options: Options::default(),
            mixed: false,
        },
        Operation::InvalidSide(0),
        Operation::InvalidSide(1),
        Operation::Save {
            forest: 1,
            side: Side::Points,
        },
        Operation::Copy {
            forest: 1,
            mode: CopyMode::TrustedRetain,
        },
        Operation::DropForest(1),
        Operation::Restore {
            forest: 1,
            saved: 0,
            retained: true,
        },
        Operation::Copy {
            forest: 1,
            mode: CopyMode::Detach,
        },
        Operation::DropForest(1),
        Operation::Explore(vec![ViewOperation::Read(0)]),
    ]));
}
