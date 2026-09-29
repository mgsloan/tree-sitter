use crate::{
    Error, FieldId, Forest, ForestRegion, Language, RegionIx, SlotIx, TreeIx,
    native::{Point, Reduction},
    side_data::{PointsData, PresenceCache},
    storage::*,
    types::{PackedPoint, SlabOffset, SquatterGrammarId, SquatterKindId},
};

mod traversal;

/// Controls slab capacity, compaction, and optional presence/point data.
///
/// **Not in Tree-sitter**
#[derive(Clone, Copy)]
pub struct PackOptions<'options> {
    pub initial_group_capacity: u32,
    pub repack: bool,
    /// Select coverage once per region after layout finalization. The default
    /// selects regions with at least 64 groups, an initial size heuristic.
    pub symbol_presence: &'options dyn Fn(ForestRegion<'_>) -> bool,
    /// Store coordinates during construction; enabling points can change grouping.
    pub points: bool,
}

impl Default for PackOptions<'_> {
    fn default() -> Self {
        Self {
            initial_group_capacity: 0,
            repack: false,
            symbol_presence: &|region| region.group_count() >= 64,
            points: true,
        }
    }
}

impl PackOptions<'_> {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A nonempty run of roots sharing an exact grammar. Packing preserves order.
/// Sorting roots by start byte accelerates bounded queries; nonoverlapping roots
/// also allow seeking to the first relevant tree. Neither establishes a shared source.
pub struct PackRegion<'tree> {
    pub language: Language,
    pub roots: Vec<tree_sitter::Node<'tree>>,
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
#[derive(Default)]
pub struct Packer {
    traversal: traversal::Traversal,
}

impl Packer {
    /// Creates empty reusable packing scratch.
    pub fn new() -> Result<Self, Error> {
        Ok(Self::default())
    }

    /// Packs a tree with default options while reusing scratch.
    pub fn pack(&mut self, language: &Language, tree: &tree_sitter::Tree) -> Result<Forest, Error> {
        self.pack_with_options(language, tree, PackOptions::default())
    }

    /// Packs a tree with selected storage and side-data options
    /// while reusing scratch.
    pub fn pack_with_options(
        &mut self,
        language: &Language,
        tree: &tree_sitter::Tree,
        options: PackOptions<'_>,
    ) -> Result<Forest, Error> {
        let (forest, _) = self.pack_forest(
            vec![PackRegion {
                language: language.clone(),
                roots: vec![tree.root_node()],
            }],
            options,
        )?;
        Ok(forest)
    }

    /// Packs caller-defined regions without merging inputs or coordinate frames.
    pub fn pack_forest(
        &mut self,
        inputs: Vec<PackRegion<'_>>,
        options: PackOptions<'_>,
    ) -> Result<(Forest, Vec<TreeIx>), Error> {
        if inputs.iter().any(|input| input.roots.is_empty()) {
            return Err(Error::InvalidArgument);
        }
        let languages: Vec<_> = inputs.iter().map(|input| input.language.clone()).collect();
        let mut expected_nodes = 0u64;
        for input in &inputs {
            for root in &input.roots {
                expected_nodes = expected_nodes
                    .checked_add(root.descendant_count() as u64)
                    .ok_or(Error::Overflow)?;
            }
        }
        let expected_nodes = u32::try_from(expected_nodes).map_err(|_| Error::Overflow)?;
        let capacity = if inputs.is_empty() {
            0
        } else {
            initial_capacity(expected_nodes, &options)
        };
        let mut builder = Builder::new_forest(&languages, capacity, options.points)?;
        let mut mapping = Vec::new();
        for (index, input) in inputs.into_iter().enumerate() {
            let region = RegionIx(index as u32);
            for node in input.roots {
                let root = traversal::Root::new(node, input.language.tables(), options.points)?;
                let start = builder.distance();
                traversal::pack(
                    &mut builder,
                    input.language.tables(),
                    &mut self.traversal,
                    root,
                )?;
                let tree = builder.finish_root(start, region)?;
                mapping.try_reserve(1).map_err(|_| Error::Allocation)?;
                mapping.push(tree);
            }
        }
        Ok((builder.finish(options)?, mapping))
    }

    pub(crate) fn pack_reductions(
        &mut self,
        language: &Language,
        nodes: &[Reduction],
        root: u32,
        options: PackOptions<'_>,
    ) -> Result<Forest, Error> {
        let mut builder = Builder::for_input(
            language,
            nodes[root as usize].visible_descendant_count + 1,
            &options,
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
    pub fn drop_scratch(&mut self) {
        self.traversal.drop_scratch();
    }
}

impl Forest {
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
        options: PackOptions<'_>,
    ) -> Result<Self, Error> {
        Packer::new()?.pack_with_options(language, tree, options)
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
        options: PackOptions<'_>,
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
    forest: Forest,
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
        options: &PackOptions<'_>,
    ) -> Result<Self, Error> {
        Self::new(
            language,
            initial_capacity(expected_nodes, options),
            options.points,
        )
    }

    fn new(language: &Language, capacity: u32, points: bool) -> Result<Self, Error> {
        Self::new_forest(std::slice::from_ref(language), capacity, points)
    }

    fn new_forest(languages: &[Language], capacity: u32, points: bool) -> Result<Self, Error> {
        let forest = Forest::empty(languages, capacity)?;
        let points = points.then(|| PointsData::empty(&forest)).transpose()?;
        Ok(Self {
            forest,
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
            if self.count == 0 && self.slot_base > u32::MAX - GROUP_SIZE {
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

            if self.count == 0 && self.forest.group_count() == self.forest.group_capacity() {
                let capacity = self
                    .forest
                    .group_capacity()
                    .checked_mul(2)
                    .ok_or(Error::Overflow)?;
                self.forest.resize(capacity, self.forest.data().flags())?;
            }

            let slot = self.distance();
            if slot.is_multiple_of(GROUP_SIZE)
                && let Some(points) = &mut self.points
            {
                points.grow(slot / GROUP_SIZE + 1)?;
            }
            let data = self.forest.data_mut();
            let layout = data.layout;
            let mut writer = data.writer();
            if layout.symbol_width == 1 {
                writer.put_byte(layout.symbol, slot, event.symbol.raw() as u8);
            } else {
                writer.put_short(layout.symbol, slot, event.symbol.raw());
            }
            if layout.symbol_width == 1 {
                writer.put_byte(layout.grammar, slot, event.grammar.raw() as u8);
            } else {
                writer.put_short(layout.grammar, slot, event.grammar.raw());
            }
            writer.put_short(layout.field, slot, event.field.map_or(0, FieldId::raw));
            if event.grammar.raw() != event.symbol.raw() {
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

        let data = self.forest.data_mut();
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

    fn finish_root(&mut self, start: u32, region: RegionIx) -> Result<TreeIx, Error> {
        self.close();
        let data = self.forest.data_mut();
        let index = TreeIx::from_raw(u32::try_from(data.trees.len()).map_err(|_| Error::Overflow)?);
        let end = TreeIx::from_raw(index.raw().checked_add(1).ok_or(Error::Overflow)?);
        data.trees.try_reserve(1).map_err(|_| Error::Allocation)?;
        data.trees.push(TreeData {
            region,
            slots: SlotIx(start)..SlotIx(self.slot_base),
        });
        let region = &mut data.regions[region.raw() as usize];
        if region.trees.is_empty() {
            region.trees.start = index;
            region.slots.start = SlotIx(start);
        }
        region.trees.end = end;
        region.slots.end = SlotIx(self.slot_base);
        Ok(index)
    }

    fn finish(mut self, options: PackOptions<'_>) -> Result<Forest, Error> {
        self.close();
        if self.forest.data().trees.is_empty() && self.forest.group_count() != 0 {
            self.finish_root(0, RegionIx(0))?;
        }
        let groups = self.forest.group_count();
        let capacity = if options.repack {
            groups
        } else {
            self.forest.group_capacity()
        };
        self.forest.finish_layout(capacity, self.optional)?;
        self.forest.classify_regions();
        if let Some(cache) =
            PresenceCache::build_for_packing(&self.forest, options.symbol_presence)?
        {
            self.forest.set_presence_cache(cache)?;
        }
        if let Some(points) = self.points {
            self.forest.set_point_data(points)?;
        }
        Ok(self.forest)
    }
}

fn initial_capacity(expected_nodes: u32, options: &PackOptions<'_>) -> u32 {
    if options.initial_group_capacity == 0 {
        expected_nodes / (GROUP_SIZE * 3 / 4) + 1
    } else {
        options.initial_group_capacity
    }
}

#[cfg(test)]
#[path = "../tests/support/internal.rs"]
mod tests;
