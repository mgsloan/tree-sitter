use crate::native::{CompiledQuery, PatternEntry, Range, flags::*};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PresenceRequirement {
    pub symbol: u16,
    pub field: u16,
}

#[derive(Clone, Copy, Default)]
pub(crate) enum Relation {
    #[default]
    Root,
    FirstNamedChild,
    NextNamedSibling,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct DirectStep {
    pub relation: Relation,
    pub symbol_start: u16,
    pub symbol_span: u16,
    pub field: u16,
    pub last_named_child: bool,
}

pub(crate) struct DirectPlan {
    pub steps: Vec<DirectStep>,
    pub roots: Vec<u64>,
    pub start_steps: [u16; 64],
    pub end_steps: [u16; 64],
    pub local_patterns: u64,
}

#[derive(Default)]
pub(crate) struct SymbolFilter {
    pub matches: Vec<(u16, u16)>,
}

pub(crate) struct Program {
    pub pattern_map: Vec<Range>,
    pub scan_symbols: Vec<u64>,
    pub scan_targets: Vec<u16>,
    pub scan_filter: SymbolFilter,
    pub presence: Vec<PresenceRequirement>,
    pub direct: Option<DirectPlan>,
    pub needs_fields: bool,
    pub needs_supertypes: bool,
    pub repeated_captures: bool,
}

impl Program {
    pub fn new(compiled: &mut CompiledQuery) -> Self {
        // Derived inline fields must not survive a rebuild after native mutation.
        for step in compiled.steps_mut() {
            step.flags &= !IS_LOCAL;
        }
        for entry in compiled.entries_mut() {
            entry.presence_requirement = 0;
        }

        let mut result = Self {
            pattern_map: vec![
                Range {
                    offset: 0,
                    length: 0
                };
                compiled.view.symbol_count as usize + 1
            ],
            scan_symbols: Vec::new(),
            scan_targets: Vec::new(),
            scan_filter: SymbolFilter::default(),
            presence: Vec::new(),
            direct: None,
            needs_fields: compiled.steps().iter().any(|step| step.field != 0),
            needs_supertypes: compiled
                .steps()
                .iter()
                .any(|step| step.supertype_symbol != 0),
            repeated_captures: unsafe { compiled.view.capture_quantifiers.as_slice() }
                .iter()
                .any(|captures| {
                    unsafe { captures.as_slice() }
                        .iter()
                        .any(|quantifier| *quantifier >= 3)
                }),
        };

        for index in 0..compiled.entries().len() {
            let entry = compiled.entries()[index];
            if entry.flags & 1 != 0 && local_alternative(compiled, entry) {
                compiled.steps_mut()[entry.step_index as usize].flags |= IS_LOCAL;
            }

            if index >= compiled.view.wildcard_root_pattern_count as usize {
                let symbol = compiled.steps()[entry.step_index as usize].symbol as usize;
                let range = &mut result.pattern_map[symbol];
                if range.length == 0 {
                    range.offset = index as u32;
                }
                range.length += 1;
            }

            if let Some(requirement) = presence_requirement(compiled, entry) {
                let requirement_index = result
                    .presence
                    .iter()
                    .position(|stored| *stored == requirement)
                    .unwrap_or(result.presence.len());
                if requirement_index < u16::MAX as usize {
                    if requirement_index == result.presence.len() {
                        result.presence.push(requirement);
                    }
                    compiled.entries_mut()[index].presence_requirement =
                        requirement_index as u16 + 1;
                }
            }
        }

        result.direct = DirectPlan::new(compiled);
        result.prepare_scan(compiled);
        result
    }

    pub fn disable_pattern(&mut self, compiled: &CompiledQuery, pattern: usize) {
        if let Some(plan) = &mut self.direct {
            let keep = !(1 << pattern);
            for roots in &mut plan.roots {
                *roots &= keep;
            }
        }

        // Native removal compacts entries, so their indexed ranges must change.
        // Remaining step/entry annotations and direct-plan topology still hold.
        // Scan filters may retain removed roots: extra candidates are harmless,
        // and keeping them matches the reference's mutation behavior.
        self.pattern_map.fill(Range {
            offset: 0,
            length: 0,
        });
        for (index, entry) in compiled
            .entries()
            .iter()
            .enumerate()
            .skip(compiled.view.wildcard_root_pattern_count as usize)
        {
            let symbol = compiled.steps()[entry.step_index as usize].symbol as usize;
            let range = &mut self.pattern_map[symbol];
            if range.length == 0 {
                range.offset = index as u32;
            }
            range.length += 1;
        }
    }

    fn prepare_scan(&mut self, compiled: &CompiledQuery) {
        // Root ?/* alternatives include an empty wildcard branch; every node
        // can start such a match, so symbol skipping would lose empty matches.
        if compiled
            .entries()
            .iter()
            .any(|entry| compiled.steps()[entry.step_index as usize].symbol == 0)
        {
            return;
        }

        let count = compiled.view.symbol_count;
        self.scan_symbols
            .resize((count as usize + 2).div_ceil(64), 0);
        for symbol in 1..=count {
            if self.pattern_map[symbol as usize].length != 0 {
                self.scan_symbols[symbol as usize / 64] |= 1 << (symbol % 64);
                self.scan_targets.push(symbol as u16);
            }
        }
        self.scan_filter = SymbolFilter::new(&self.scan_targets);
    }
}

fn local_alternative(compiled: &CompiledQuery, entry: PatternEntry) -> bool {
    let step = compiled.steps()[entry.step_index as usize];
    let pattern = compiled.patterns()[entry.pattern_index.get() as usize];
    let end = pattern.steps.end() - 1;
    if step.depth != 0
        || step.field != 0
        || step.supertype_symbol != 0
        || step.negated_field_list_id != 0
        || step.has(
            IS_IMMEDIATE
                | IS_LAST_CHILD
                | IS_MISSING
                | IS_DEAD_END
                | IS_PASS_THROUGH
                | ALTERNATIVE_IS_SKIP,
        )
    {
        return false;
    }

    let mut next = entry.step_index as usize + 1;
    for _ in 0..pattern.steps.length {
        if next == end {
            return true;
        }
        if next >= end || !compiled.steps()[next].has(IS_DEAD_END) {
            return false;
        }
        next = compiled.steps()[next].alternative_index as usize;
    }
    false
}

fn presence_requirement(
    compiled: &CompiledQuery,
    entry: PatternEntry,
) -> Option<PresenceRequirement> {
    let root = compiled.steps()[entry.step_index as usize];
    if entry.flags & 1 == 0 || root.depth != 0 || root.alternative_index != u16::MAX {
        return None;
    }

    let mut required = None;
    let pattern = compiled.patterns()[entry.pattern_index.get() as usize];
    for step in &compiled.steps()[entry.step_index as usize + 1..pattern.steps.end()] {
        if step.depth == 0
            || step.depth == u16::MAX
            || step.alternative_index != u16::MAX
            || step.has(IS_DEAD_END | IS_PASS_THROUGH | IS_MISSING)
        {
            break;
        }
        if (step.symbol != 0 && step.symbol as u32 != compiled.view.symbol_count) || step.field != 0
        {
            required = Some(PresenceRequirement {
                symbol: step.symbol,
                field: step.field,
            });
        }
    }
    required
}

impl DirectPlan {
    fn new(compiled: &CompiledQuery) -> Option<Self> {
        if compiled.patterns().len() > 64 {
            return None;
        }
        let mut plan = Self {
            // Root symbols, including local alternatives, are checked by dispatch.
            steps: vec![
                DirectStep {
                    symbol_span: u16::MAX,
                    ..Default::default()
                };
                compiled.steps().len()
            ],
            roots: Vec::new(),
            start_steps: [0; 64],
            end_steps: [0; 64],
            local_patterns: 0,
        };
        let mut patterns = 0;
        let tables = compiled.language.tables();

        for (index, entry) in compiled.entries().iter().copied().enumerate() {
            if entry.flags & 1 == 0 {
                return None;
            }
            let pattern = compiled.patterns()[entry.pattern_index.get() as usize];
            let bit = 1 << entry.pattern_index.get();
            let end = pattern.steps.end() - 1;
            plan.end_steps[entry.pattern_index.get() as usize] = end as u16;

            let root = compiled.steps()[entry.step_index as usize];
            if root.symbol != 0 && local_alternative(compiled, entry) {
                if patterns & bit != 0 {
                    let first = compiled.steps()
                        [plan.start_steps[entry.pattern_index.get() as usize] as usize];
                    if plan.local_patterns & bit == 0
                        || first.capture_ids != root.capture_ids
                        || compiled.entries()[..index].iter().any(|other| {
                            other.pattern_index == entry.pattern_index
                                && compiled.steps()[other.step_index as usize].symbol == root.symbol
                        })
                    {
                        return None;
                    }
                } else {
                    plan.start_steps[entry.pattern_index.get() as usize] = entry.step_index;
                }
                patterns |= bit;
                plan.local_patterns |= bit;
                continue;
            }

            if entry.step_index as u32 != pattern.steps.offset
                || patterns & bit != 0
                || end == pattern.steps.offset as usize
                || compiled.steps()[end].depth != u16::MAX
            {
                return None;
            }
            patterns |= bit;
            plan.start_steps[entry.pattern_index.get() as usize] = entry.step_index;

            for step_index in pattern.steps.offset as usize..end {
                let step = compiled.steps()[step_index];
                let root = step_index == pattern.steps.offset as usize;
                if step.alternative_index != u16::MAX
                    || step.supertype_symbol != 0
                    || step.negated_field_list_id != 0
                    || step.has(
                        IS_PASS_THROUGH
                            | IS_DEAD_END
                            | IS_INSIDE_ALTERNATION
                            | IS_MISSING
                            | ALTERNATIVE_IS_SKIP,
                    )
                {
                    return None;
                }
                if root {
                    if step.depth != 0 || step.field != 0 || step.has(IS_IMMEDIATE | IS_LAST_CHILD)
                    {
                        return None;
                    }
                } else {
                    // Named anchors select one possible child at each step;
                    // these plans need neither branching nor longest-match dedup.
                    let named = if step.symbol != 0 {
                        step.symbol as u32 == compiled.view.symbol_count
                            || tables.named_index(crate::SquatterKindId(step.symbol))
                    } else {
                        step.has(IS_NAMED)
                    };
                    if step.depth != 1 || !step.has(IS_IMMEDIATE) || !named {
                        return None;
                    }
                }
                let (symbol_start, symbol_span) = if root {
                    (0, u16::MAX)
                } else if step.symbol == 0 {
                    // Named child traversal excludes the other reserved kind, _ERROR.
                    (0, compiled.view.symbol_count as u16 - 1)
                } else {
                    (step.symbol, 0)
                };
                plan.steps[step_index] = DirectStep {
                    relation: if root {
                        Relation::Root
                    } else if step_index == pattern.steps.offset as usize + 1 {
                        Relation::FirstNamedChild
                    } else {
                        Relation::NextNamedSibling
                    },
                    symbol_start,
                    symbol_span,
                    field: step.field,
                    last_named_child: step.has(IS_LAST_CHILD),
                };
            }
        }

        // Unsupported plans need no root table.
        plan.roots
            .resize(compiled.view.symbol_count as usize + 2, 0);
        for symbol in 1..=compiled.view.symbol_count as usize {
            for entry in compiled.entries() {
                let step = compiled.steps()[entry.step_index as usize];
                if if step.symbol != 0 {
                    step.symbol == symbol as u16
                } else {
                    symbol != compiled.view.symbol_count as usize
                        && (!step.has(IS_NAMED)
                            || tables.named_index(crate::SquatterKindId(symbol as u16)))
                } {
                    plan.roots[symbol] |= 1 << entry.pattern_index.get();
                }
            }
        }
        Some(plan)
    }
}

impl SymbolFilter {
    fn new(targets: &[u16]) -> Self {
        if targets.is_empty() || targets.len() > 32 {
            return Self::default();
        }
        let mut matches: Vec<_> = targets.iter().map(|target| (*target, u16::MAX)).collect();

        // Merge disjoint halves of an exact symbol set. Equal masks differing
        // by one value bit can drop that bit without admitting extra symbols.
        loop {
            let mut merged = false;
            'search: for index in 0..matches.len() {
                for other in index + 1..matches.len() {
                    let difference = matches[index].0 ^ matches[other].0;
                    if matches[index].1 == matches[other].1 && difference.is_power_of_two() {
                        matches[index].0 &= !difference;
                        matches[index].1 &= !difference;
                        matches.remove(other);
                        merged = true;
                        break 'search;
                    }
                }
            }
            if !merged {
                break;
            }
        }
        if matches.len() > 8 {
            Self::default()
        } else {
            Self { matches }
        }
    }
}
