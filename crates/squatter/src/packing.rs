use crate::{
    Error, FieldId, Language, Tree,
    native::{Point, Reduction},
    side_data::{PointsData, PresenceCache},
    storage::*,
    types::{PackedPoint, SlabOffset, SquatterGrammarId, SquatterKindId},
};

mod traversal;

/// Controls slab capacity, compaction, and optional presence/point
/// data when packing.
///
/// **Not in Tree-sitter**
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PackOptions {
    pub initial_group_capacity: u32,
    pub repack: bool,
    pub symbol_presence: bool,
    /// Store coordinates during construction; enabling points can change grouping.
    pub points: bool,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            initial_group_capacity: 0,
            repack: false,
            symbol_presence: true,
            points: true,
        }
    }
}

struct InputNode {
    start_byte: u32,
    end_byte: u32,
    start_point: Point,
    end_point: Point,
    symbol: SquatterKindId,
    grammar: SquatterGrammarId,
    field: Option<FieldId>,
    supertype: u16,
    flags: u16,
}

/// Reusable traversal scratch for packing multiple trees. It does
/// not retain input trees after packing.
///
/// **Not in Tree-sitter**
pub struct PackContext {
    traversal: traversal::Traversal,
}

impl PackContext {
    /// Creates empty reusable packing scratch.
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            traversal: traversal::Traversal::default(),
        })
    }

    /// Packs a tree with default options while reusing scratch.
    pub fn pack(&mut self, language: &Language, tree: &tree_sitter::Tree) -> Result<Tree, Error> {
        self.pack_with_options(language, tree, PackOptions::default())
    }

    /// Packs a tree with selected storage and side-data options
    /// while reusing scratch.
    pub fn pack_with_options(
        &mut self,
        language: &Language,
        tree: &tree_sitter::Tree,
        options: PackOptions,
    ) -> Result<Tree, Error> {
        let root = traversal::Root::new(tree, language.tables())?;
        let mut builder = Builder::for_input(language, root.expected_nodes, options)?;
        traversal::pack(&mut builder, language.tables(), &mut self.traversal, root)?;
        builder.finish(options)
    }

    pub(crate) fn pack_reductions(
        &mut self,
        language: &Language,
        nodes: &[Reduction],
        root: u32,
        options: PackOptions,
    ) -> Result<Tree, Error> {
        let mut builder = Builder::for_input(
            language,
            nodes[root as usize].visible_descendant_count + 1,
            options,
        )?;
        traversal::pack_reductions(
            &mut builder,
            language.tables(),
            &mut self.traversal,
            nodes,
            root,
        )?;
        builder.finish(options)
    }

    /// Releases retained traversal scratch.
    pub fn trim(&mut self) {
        self.traversal.trim();
    }
}

impl Tree {
    /// Packs a tree-sitter snapshot with default options.
    ///
    /// **Not in Tree-sitter**
    pub fn pack(language: &Language, tree: &tree_sitter::Tree) -> Result<Self, Error> {
        Self::pack_with_options(language, tree, PackOptions::default())
    }

    /// Packs a snapshot with selected storage and side-data
    /// options.
    ///
    /// **Not in Tree-sitter**
    pub fn pack_with_options(
        language: &Language,
        tree: &tree_sitter::Tree,
        options: PackOptions,
    ) -> Result<Self, Error> {
        PackContext::new()?.pack_with_options(language, tree, options)
    }

    /// Parses using the supplied tree-sitter parser, then packs the
    /// fresh snapshot.
    ///
    /// **Not in Tree-sitter**
    pub fn parse(
        language: &Language,
        parser: &mut tree_sitter::Parser,
        source: impl AsRef<[u8]>,
    ) -> Result<Self, Error> {
        Self::parse_with_options(language, parser, source, PackOptions::default())
    }

    /// Parses using the supplied tree-sitter parser, then packs
    /// with selected options. Input exceeding the byte-offset representation limit is
    /// rejected.
    ///
    /// **Not in Tree-sitter**
    pub fn parse_with_options(
        language: &Language,
        parser: &mut tree_sitter::Parser,
        source: impl AsRef<[u8]>,
        options: PackOptions,
    ) -> Result<Self, Error> {
        let source = source.as_ref();
        if source.len() > u32::MAX as usize {
            return Err(Error::Overflow);
        }
        let tree = parser.parse(source, None).ok_or(Error::InvalidArgument)?;
        Self::pack_with_options(language, &tree, options)
    }
}

#[derive(Clone, Copy, Default)]
struct Values {
    start_row: u32,
    end_row: u32,
    start_column: u32,
    end_column: u32,
    span: u32,
    start_byte: u32,
    end_byte: u32,
}

#[derive(Clone, Copy, Default)]
struct Pending {
    values: Values,
    supertype: u16,
}

struct Builder {
    tree: Tree,
    pending: [Pending; GROUP_SIZE as usize],
    count: u32,
    slot_base: u32,
    base: Values,
    maximum: Values,
    last: u64,
    extra: u64,
    missing: u64,
    error: u64,
    optional: u32,
    points: Option<PointsData>,
}

impl Builder {
    fn for_input(
        language: &Language,
        expected_nodes: u32,
        options: PackOptions,
    ) -> Result<Self, Error> {
        let capacity = if options.initial_group_capacity == 0 {
            expected_nodes / (GROUP_SIZE * 3 / 4) + 1
        } else {
            options.initial_group_capacity
        };
        Self::new(language, capacity, options.points)
    }

    fn new(language: &Language, capacity: u32, points: bool) -> Result<Self, Error> {
        let tree = Tree::empty(language, capacity)?;
        let points = points.then(|| PointsData::empty(&tree)).transpose()?;
        Ok(Self {
            tree,
            pending: [Pending::default(); GROUP_SIZE as usize],
            count: 0,
            slot_base: 0,
            base: Values::default(),
            maximum: Values::default(),
            last: 0,
            extra: 0,
            missing: 0,
            error: 0,
            optional: 0,
            points,
        })
    }

    fn distance(&self) -> u32 {
        self.slot_base + self.count
    }

    #[inline(always)]
    fn extend(&mut self, value: Values) -> bool {
        if self.count == 0 {
            self.base = value;
            self.maximum = value;
            return true;
        }

        let mut base = self.base;
        let mut maximum = self.maximum;
        let extend = |value: u32, base: &mut u32, maximum: &mut u32, limit: u32| {
            *base = (*base).min(value);
            *maximum = (*maximum).max(value);
            *maximum - *base <= limit
        };

        // Reverse preorder makes start bytes nonincreasing; end bytes need both extrema.
        base.start_byte = value.start_byte;
        if maximum.start_byte - base.start_byte > 255
            || !extend(
                value.span,
                &mut base.span,
                &mut maximum.span,
                (1 << SPAN_BITS) - 1,
            )
            || !extend(
                value.end_byte,
                &mut base.end_byte,
                &mut maximum.end_byte,
                65535,
            )
        {
            return false;
        }

        if self.points.is_some()
            && (!extend(
                value.start_row,
                &mut base.start_row,
                &mut maximum.start_row,
                255,
            ) || !extend(value.end_row, &mut base.end_row, &mut maximum.end_row, 255)
                || !extend(
                    value.start_column,
                    &mut base.start_column,
                    &mut maximum.start_column,
                    255,
                )
                || !extend(
                    value.end_column,
                    &mut base.end_column,
                    &mut maximum.end_column,
                    255,
                ))
        {
            return false;
        }

        self.base = base;
        self.maximum = maximum;
        true
    }

    #[inline(always)]
    fn emit(&mut self, event: &InputNode, boundary: u32) -> Result<(), Error> {
        loop {
            if self.count == GROUP_SIZE {
                self.close();
            }
            if self.distance() >= u32::MAX - GROUP_SIZE {
                return Err(Error::Overflow);
            }

            // Closing a partial group adds physical waste slots. Recompute the
            // span on each attempt so ancestors include that waste.
            let value = Values {
                start_row: event.start_point.row,
                end_row: event.end_point.row,
                start_column: event.start_point.column,
                end_column: event.end_point.column,
                span: self.distance() - boundary,
                start_byte: event.start_byte,
                end_byte: event.end_byte,
            };
            if !self.extend(value) {
                self.close();
                continue;
            }

            if self.count == 0 && self.tree.group_count() == self.tree.group_capacity() {
                let capacity = self
                    .tree
                    .group_capacity()
                    .checked_mul(2)
                    .ok_or(Error::Overflow)?;
                self.tree.resize(capacity, self.tree.data().flags())?;
            }

            let slot = self.distance();
            if slot % GROUP_SIZE == 0
                && let Some(points) = &mut self.points
            {
                points.grow(slot / GROUP_SIZE + 1)?;
            }
            let data = self.tree.data_mut();
            let layout = data.layout;
            let default_grammar = data.tables().default_grammar(event.symbol);
            let mut writer = data.writer();
            if layout.symbol_width == 1 {
                writer.put_byte(layout.symbol, slot, event.symbol.get() as u8);
            } else {
                writer.put_short(layout.symbol, slot, event.symbol.get());
            }
            if layout.grammar_width == 1 {
                writer.put_byte(layout.grammar, slot, event.grammar.get() as u8);
            } else {
                writer.put_short(layout.grammar, slot, event.grammar.get());
            }
            writer.put_short(layout.field, slot, event.field.map_or(0, FieldId::get));
            if event.grammar != default_grammar {
                self.optional |= SEPARATE_GRAMMAR;
            }

            self.pending[self.count as usize] = Pending {
                values: value,
                supertype: event.supertype,
            };
            self.last |= ((event.flags & 1 != 0) as u64) << self.count;
            self.extra |= ((event.flags & 2 != 0) as u64) << self.count;
            self.missing |= ((event.flags & 4 != 0) as u64) << self.count;
            self.error |= ((event.flags & 8 != 0) as u64) << self.count;
            self.count += 1;
            return Ok(());
        }
    }

    fn close(&mut self) {
        if self.count == 0 {
            return;
        }

        let data = self.tree.data_mut();
        let group = data.groups();
        let layout = data.layout;
        data.put_word(SlabOffset(0), 1, group + 1);
        data.put_short(layout.waste, group, (GROUP_SIZE - self.count) as u16);
        data.put_word(layout.span_max, group, self.maximum.span);
        data.put_word(layout.start_byte_base, group, self.base.start_byte);
        data.put_word(layout.end_byte_base, group, self.maximum.end_byte);

        for (offset, flags) in [
            (layout.last, self.last),
            (layout.extra, self.extra),
            (layout.error, self.error),
            (layout.missing, self.missing),
        ] {
            match GROUP_SIZE {
                16 => data.put_short(offset, group, flags as u16),
                32 => data.put_word(offset, group, flags as u32),
                64 => data.put_long(offset, group, flags),
                _ => unreachable!(),
            }
        }
        if self.extra != 0 {
            self.optional |= EXTRAS;
        }
        if self.missing != 0 {
            self.optional |= MISSING;
        }
        if self.error != 0 {
            self.optional |= ERRORS;
        }

        if let Some(points) = &mut self.points {
            points.put_bases(
                group,
                PackedPoint(
                    (u64::from(self.base.start_row) << 32) | u64::from(self.base.start_column),
                ),
                PackedPoint(
                    (u64::from(self.maximum.end_row) << 32) | u64::from(self.maximum.end_column),
                ),
            );
        }

        let mut writer = data.writer();
        let pending = &self.pending[..self.count as usize];
        for (index, pending) in pending.iter().enumerate() {
            let value = pending.values;
            let slot = self.slot_base + index as u32;
            if SPAN_BITS == 16 {
                writer.put_short(
                    layout.span_delta,
                    slot,
                    (self.maximum.span - value.span) as u16,
                );
            } else {
                writer.put_byte(
                    layout.span_delta,
                    slot,
                    (self.maximum.span - value.span) as u8,
                );
            }
            writer.put_byte(
                layout.start_byte_delta,
                slot,
                (value.start_byte - self.base.start_byte) as u8,
            );
            writer.put_short(
                layout.end_byte_delta,
                slot,
                (self.maximum.end_byte - value.end_byte) as u16,
            );
            writer.put_short(layout.supertype, slot, pending.supertype);
            if let Some(points) = &mut self.points {
                points.put_deltas(
                    slot,
                    (((value.start_row - self.base.start_row) << 8)
                        | (value.start_column - self.base.start_column)) as u16,
                    (((self.maximum.end_row - value.end_row) << 8)
                        | (self.maximum.end_column - value.end_column)) as u16,
                );
            }
        }

        self.count = 0;
        self.slot_base += GROUP_SIZE;
        self.last = 0;
        self.extra = 0;
        self.missing = 0;
        self.error = 0;
    }

    fn finish(mut self, options: PackOptions) -> Result<Tree, Error> {
        self.close();
        let groups = self.tree.group_count();
        let capacity = if options.repack {
            groups
        } else {
            self.tree.group_capacity()
        };
        self.tree.finish_layout(capacity, self.optional)?;
        if options.symbol_presence {
            let cache = PresenceCache::build(&self.tree)?;
            self.tree.set_presence_cache(cache)?;
        }
        if let Some(points) = self.points {
            self.tree.set_point_data(points)?;
        }
        Ok(self.tree)
    }
}

#[cfg(test)]
#[path = "../tests/support/internal.rs"]
mod tests;
