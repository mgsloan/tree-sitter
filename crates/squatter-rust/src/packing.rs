use crate::{
    Error, FieldId, Grammar, Tree,
    native::{Point, Reduction},
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
    presence: PresenceScratch,
}

impl PackContext {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            traversal: traversal::Traversal::default(),
            presence: PresenceScratch::default(),
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
        builder.finish(&mut self.presence, options)
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
        builder.finish(&mut self.presence, options)
    }

    pub fn trim(&mut self) {
        self.traversal.trim();
        self.presence.trim();
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
    start_row: u32,
    end_row: u32,
    start_column: u32,
    end_column: u32,
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
    points: bool,
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
        Ok(Self {
            tree: Tree::empty(grammar, capacity, points)?,
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

        // Reverse preorder makes start bytes and rows nonincreasing; columns
        // and end positions still need both extrema.
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

        if self.points {
            base.start_row = value.start_row;
            if maximum.start_row - base.start_row > 255
                || !extend(value.end_row, &mut base.end_row, &mut maximum.end_row, 255)
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
                )
            {
                return false;
            }
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
                start_row: event.start_point.row,
                end_row: event.end_point.row,
                start_column: event.start_point.column,
                end_column: event.end_point.column,
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
            let data = self.tree.data_mut();
            data.put_short(data.layout.symbol, slot, code.get());
            data.put_short(data.layout.field, slot, event.field.map_or(0, FieldId::get));
            if data.tables().separate != 0 {
                data.put_short(data.layout.grammar, slot, event.grammar.get());
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
        if self.points {
            data.put_long(
                layout.start_point_base,
                group,
                (self.base.start_row as u64) << 32 | self.base.start_column as u64,
            );
            data.put_long(
                layout.end_point_base,
                group,
                (self.maximum.end_row as u64) << 32 | self.maximum.end_column as u64,
            );
        }

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
        for index in 0..self.count {
            let pending = self.pending[index as usize];
            let value = pending.values;
            let slot = self.slot_base + index;
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
            if self.points {
                writer.put_short(
                    layout.start_point,
                    slot,
                    (((value.start_row - self.base.start_row) << 8)
                        | (value.start_column - self.base.start_column)) as u16,
                );
                writer.put_short(
                    layout.end_point,
                    slot,
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
        self.has_error = false;
    }

    fn finish(
        mut self,
        scratch: &mut PresenceScratch,
        options: PackOptions,
    ) -> Result<Tree, Error> {
        self.close();
        let groups = self.tree.group_count();
        let trailing = if options.symbol_presence && groups > 32 {
            u32::try_from(presence_size(
                self.tree.data().tables().symbol_count + 2,
                groups,
            ))
            .map_err(|_| Error::Overflow)?
        } else {
            0
        };
        let capacity = if options.repack {
            groups
        } else {
            self.tree.group_capacity()
        };
        let flags = (self.tree.data().flags() & !OPTIONAL) | self.optional;
        self.tree.finish_layout(capacity, flags, trailing)?;
        if options.symbol_presence {
            self.tree.build_presence(scratch);
        }
        Ok(self.tree)
    }
}
