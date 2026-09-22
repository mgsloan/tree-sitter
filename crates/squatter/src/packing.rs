use crate::{
    Error, FieldId, Grammar, Tree,
    native::{Point, Reduction},
    side_data::{PointData, PresenceCache},
    storage::*,
    types::{RemappedGrammarKindId, RemappedKindId, SlabOffset},
};

mod traversal;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PackOptions {
    pub initial_group_capacity: u32,
    pub repack: bool,
    pub symbol_presence: bool,
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
    symbol: RemappedKindId,
    grammar: RemappedGrammarKindId,
    field: Option<FieldId>,
    supertype: u16,
    flags: u16,
}

pub struct PackContext {
    traversal: traversal::Traversal,
}

impl PackContext {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            traversal: traversal::Traversal::default(),
        })
    }

    pub fn pack(&mut self, grammar: &Grammar, tree: &tree_sitter::Tree) -> Result<Tree, Error> {
        self.pack_with_options(grammar, tree, PackOptions::default())
    }

    pub fn pack_with_options(
        &mut self,
        grammar: &Grammar,
        tree: &tree_sitter::Tree,
        options: PackOptions,
    ) -> Result<Tree, Error> {
        let root = traversal::Root::new(tree, grammar.tables())?;
        let mut builder = Builder::for_input(grammar, root.expected_nodes, options)?;
        traversal::pack(&mut builder, grammar.tables(), &mut self.traversal, root)?;
        builder.finish(options)
    }

    pub(crate) fn pack_reductions(
        &mut self,
        grammar: &Grammar,
        nodes: &[Reduction],
        root: u32,
        options: PackOptions,
    ) -> Result<Tree, Error> {
        let mut builder = Builder::for_input(
            grammar,
            nodes[root as usize].visible_descendant_count + 1,
            options,
        )?;
        traversal::pack_reductions(
            &mut builder,
            grammar.tables(),
            &mut self.traversal,
            nodes,
            root,
        )?;
        builder.finish(options)
    }

    pub fn trim(&mut self) {
        self.traversal.trim();
    }
}

impl Tree {
    pub fn pack(grammar: &Grammar, tree: &tree_sitter::Tree) -> Result<Self, Error> {
        Self::pack_with_options(grammar, tree, PackOptions::default())
    }

    pub fn pack_with_options(
        grammar: &Grammar,
        tree: &tree_sitter::Tree,
        options: PackOptions,
    ) -> Result<Self, Error> {
        PackContext::new()?.pack_with_options(grammar, tree, options)
    }

    pub fn parse(
        grammar: &Grammar,
        parser: &mut tree_sitter::Parser,
        source: impl AsRef<[u8]>,
    ) -> Result<Self, Error> {
        Self::parse_with_options(grammar, parser, source, PackOptions::default())
    }

    pub fn parse_with_options(
        grammar: &Grammar,
        parser: &mut tree_sitter::Parser,
        source: impl AsRef<[u8]>,
        options: PackOptions,
    ) -> Result<Self, Error> {
        let source = source.as_ref();
        if source.len() > u32::MAX as usize {
            return Err(Error::Overflow);
        }
        let tree = parser.parse(source, None).ok_or(Error::InvalidArgument)?;
        Self::pack_with_options(grammar, &tree, options)
    }
}

#[derive(Clone, Copy, Default)]
struct Values {
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
    has_error: bool,
    optional: u32,
    points: Option<PointData>,
}

impl Builder {
    fn for_input(
        grammar: &Grammar,
        expected_nodes: u32,
        options: PackOptions,
    ) -> Result<Self, Error> {
        let capacity = if options.initial_group_capacity == 0 {
            expected_nodes / (GROUP_SIZE * 3 / 4) + 1
        } else {
            options.initial_group_capacity
        };
        Self::new(grammar, capacity, options.points)
    }

    fn new(grammar: &Grammar, capacity: u32, points: bool) -> Result<Self, Error> {
        let tree = Tree::empty(grammar, capacity)?;
        let points = points.then(|| PointData::empty(&tree)).transpose()?;
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
            has_error: false,
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
            || !extend(value.span, &mut base.span, &mut maximum.span, 255)
            || !extend(
                value.end_byte,
                &mut base.end_byte,
                &mut maximum.end_byte,
                65535,
            )
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
                self.tree
                    .resize(capacity, self.tree.data().flags(), 0, false)?;
            }

            let code = self
                .tree
                .data()
                .tables()
                .symbol_code(event.symbol, event.grammar)
                .ok_or(Error::Language)?;
            let slot = self.distance();
            if let Some(points) = &mut self.points {
                if slot % GROUP_SIZE == 0 {
                    points.grow(slot / GROUP_SIZE + 1)?;
                }
                points.put(
                    slot,
                    tree_sitter::Point::new(
                        event.start_point.row as usize,
                        event.start_point.column as usize,
                    ),
                    tree_sitter::Point::new(
                        event.end_point.row as usize,
                        event.end_point.column as usize,
                    ),
                )?;
            }
            let data = self.tree.data_mut();
            let layout = data.layout;
            let separate = data.tables().separate != 0;
            let mut writer = data.writer();
            writer.put_short(layout.symbol, slot, code.get());
            writer.put_short(layout.field, slot, event.field.map_or(0, FieldId::get));
            if separate {
                writer.put_short(layout.grammar, slot, event.grammar.get());
                if event.grammar.get() != code.get() {
                    self.optional |= SEPARATE_GRAMMAR;
                }
            }

            self.pending[self.count as usize] = Pending {
                values: value,
                supertype: event.supertype,
            };
            self.last |= ((event.flags & 1 != 0) as u64) << self.count;
            self.extra |= ((event.flags & 2 != 0) as u64) << self.count;
            self.missing |= ((event.flags & 4 != 0) as u64) << self.count;
            self.has_error |= event.flags & 8 != 0;
            self.count += 1;
            return Ok(());
        }
    }

    fn close(&mut self) {
        if self.count == 0 {
            return;
        }

        // Keep small spans directly readable from the delta column.
        if self.maximum.span <= 255 {
            self.base.span = 0;
        }

        let data = self.tree.data_mut();
        let group = data.groups();
        let layout = data.layout;
        data.put_word(SlabOffset(0), 1, group + 1);
        data.put_short(layout.waste, group, (GROUP_SIZE - self.count) as u16);
        data.put_word(layout.span_base, group, self.base.span);
        data.put_word(layout.start_byte_base, group, self.base.start_byte);
        data.put_word(layout.end_byte_base, group, self.maximum.end_byte);

        for (offset, flags) in [
            (layout.last, self.last),
            (layout.extra, self.extra),
            (layout.missing, self.missing),
        ] {
            match GROUP_SIZE {
                16 => data.put_short(offset, group, flags as u16),
                32 => data.put_word(offset, group, flags as u32),
                64 => data.put_long(offset, group, flags),
                _ => unreachable!(),
            }
        }
        data.put_bit(layout.error, group, self.has_error);
        if self.extra != 0 {
            self.optional |= EXTRAS;
        }
        if self.missing != 0 {
            self.optional |= MISSING;
        }
        if self.has_error {
            self.optional |= ERRORS;
        }

        let mut writer = data.writer();
        let pending = &self.pending[..self.count as usize];
        for (index, pending) in pending.iter().enumerate() {
            let value = pending.values;
            let slot = self.slot_base + index as u32;
            writer.put_byte(layout.span_delta, slot, (value.span - self.base.span) as u8);
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
        }

        self.count = 0;
        self.slot_base += GROUP_SIZE;
        self.last = 0;
        self.extra = 0;
        self.missing = 0;
        self.has_error = false;
    }

    fn finish(mut self, options: PackOptions) -> Result<Tree, Error> {
        self.close();
        let groups = self.tree.group_count();
        let capacity = if options.repack {
            groups
        } else {
            self.tree.group_capacity()
        };
        let flags = (self.tree.data().flags() & !OPTIONAL) | self.optional;
        self.tree.finish_layout(capacity, flags, 0)?;
        if options.symbol_presence {
            let cache = PresenceCache::build(&self.tree, None)?;
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
