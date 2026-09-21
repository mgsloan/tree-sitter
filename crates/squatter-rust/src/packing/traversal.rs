use super::{Builder, InputNode};
use crate::{
    Error, FieldId,
    native::{GrammarView, Point, Range, Reduction},
    types::{RemappedGrammarKindId, RemappedKindId},
};
use std::{ffi::c_void, marker::PhantomData, ptr};

#[allow(warnings, clippy::all)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/subtree.rs"));
}

#[derive(Clone, Copy, Default)]
struct Position {
    bytes: u32,
    point: Point,
}

impl Position {
    fn add(self, other: Self) -> Self {
        Self {
            bytes: self.bytes + other.bytes,
            point: Point {
                row: self.point.row + other.point.row,
                column: if other.point.row == 0 {
                    self.point.column + other.point.column
                } else {
                    other.point.column
                },
            },
        }
    }
}

impl From<ffi::Length> for Position {
    fn from(length: ffi::Length) -> Self {
        Self {
            bytes: length.bytes,
            point: Point {
                row: length.extent.row,
                column: length.extent.column,
            },
        }
    }
}

// Only the active walk reads these pointers. Frames are cleared before the
// input borrow ends; retained vector capacity never owns native subtrees.
#[derive(Clone, Copy)]
struct Subtree(*const ffi::Subtree);

struct Facts {
    children: u32,
    visible_children: u32,
    size: u32,
    padding: u32,
    visible: bool,
    extra: bool,
}

// Heap reference counts may change through other Tree owners. Read fields
// through raw pointers so no shared reference freezes the whole heap header.
// Test the inline tag without borrowing the inactive inline struct: a 32-bit
// heap pointer initializes only half of the eight-byte union.
impl Subtree {
    fn facts(self) -> Facts {
        unsafe {
            let subtree = &*self.0;
            if ffi::SubtreeInlineData::is_inline_raw(ptr::addr_of!(subtree.data)) {
                Facts {
                    children: 0,
                    visible_children: 0,
                    size: subtree.data.size_bytes.into(),
                    padding: subtree.data.padding_bytes.into(),
                    visible: subtree.data.visible(),
                    extra: subtree.data.extra(),
                }
            } else {
                let heap = subtree.ptr;
                Facts {
                    children: (*heap).child_count,
                    // Terminal-only data shares this union with child counts.
                    visible_children: if (*heap).child_count == 0 {
                        0
                    } else {
                        (*heap)
                            .__bindgen_anon_1
                            .__bindgen_anon_1
                            .visible_child_count
                    },
                    size: (*heap).size.bytes,
                    padding: (*heap).padding.bytes,
                    visible: ffi::SubtreeHeapData::visible_raw(heap),
                    extra: ffi::SubtreeHeapData::extra_raw(heap),
                }
            }
        }
    }

    fn lengths(self) -> (Position, Position, bool) {
        unsafe {
            let subtree = &*self.0;
            if ffi::SubtreeInlineData::is_inline_raw(ptr::addr_of!(subtree.data)) {
                let data = &subtree.data;
                (
                    Position {
                        bytes: data.padding_bytes.into(),
                        point: Point {
                            row: data.padding_rows().into(),
                            column: data.padding_columns.into(),
                        },
                    },
                    Position {
                        bytes: data.size_bytes.into(),
                        point: Point {
                            row: 0,
                            column: data.size_bytes.into(),
                        },
                    },
                    data.extra(),
                )
            } else {
                let heap = subtree.ptr;
                (
                    (*heap).padding.into(),
                    (*heap).size.into(),
                    ffi::SubtreeHeapData::extra_raw(heap),
                )
            }
        }
    }

    // The caller has established a nonzero child count, hence heap storage
    // and the nonterminal union member are active.
    fn branch(self) -> (*const ffi::Subtree, u16, u16) {
        unsafe {
            let heap = (*self.0).ptr;
            debug_assert!(
                !ffi::SubtreeInlineData::is_inline_raw(ptr::addr_of!((*self.0).data))
                    && (*heap).child_count != 0
            );
            (
                heap.cast::<ffi::Subtree>()
                    .sub((*heap).child_count as usize),
                (*heap).__bindgen_anon_1.__bindgen_anon_1.production_id,
                (*heap).symbol,
            )
        }
    }
}

pub(super) struct Root<'tree> {
    subtree: Subtree,
    position: Position,
    alias: u16,
    pub expected_nodes: u32,
    input: PhantomData<&'tree tree_sitter::Tree>,
}

impl<'tree> Root<'tree> {
    pub fn new(tree: &'tree tree_sitter::Tree, tables: &GrammarView) -> Result<Self, Error> {
        let root = tree.root_node();
        let expected_nodes = root.descendant_count() as u32;
        let raw = root.into_raw();
        unsafe extern "C" {
            fn ts_tree_language(tree: *const c_void) -> *const c_void;
        }
        if unsafe { ts_tree_language(raw.tree.cast()) } != tables.language {
            return Err(Error::Language);
        }
        Ok(Self {
            subtree: Subtree(raw.id.cast()),
            position: Position {
                bytes: raw.context[0],
                point: Point {
                    row: raw.context[1],
                    column: raw.context[2],
                },
            },
            alias: raw.context[3] as u16,
            expected_nodes,
            input: PhantomData,
        })
    }
}

// Inline bits for up to 64 supertypes; otherwise an offset into the mask arena.
#[derive(Clone, Copy, Default)]
struct Mask(u64);

#[derive(Clone, Copy)]
struct Node {
    position: Position,
    mask: Mask,
    boundary: u32,
    field: Option<FieldId>,
    alias: u16,
    later: bool,
}

struct Frame {
    node: Node,
    subtree: Subtree,
    children: *const ffi::Subtree,
    aliases: *const u16,
    inline_position: Position,
    child_end: u32,
    position_mark: usize,
    position_offset: usize,
    fields: Range,
    mask_mark: usize,
    child_mask: Mask,
    remaining: u32,
    structural: u32,
    visible: bool,
    child_later: bool,
}

struct ReductionFrame {
    node: Node,
    index: u32,
    next_child: u32,
    mask_mark: usize,
    child_mask: Mask,
    visible: bool,
    child_later: bool,
}

#[derive(Default)]
pub(super) struct Traversal {
    stack: Vec<Frame>,
    reductions: Vec<ReductionFrame>,
    positions: Vec<Position>,
    masks: Vec<u64>,
}

// A walk clears borrowed frames even on failure or unwind. Moving the retained
// scratch does not dereference any pointers left in its unused capacity.
unsafe impl Send for Traversal {}

impl Traversal {
    pub fn trim(&mut self) {
        *self = Self::default();
    }
}

struct Walk<'a> {
    scratch: &'a mut Traversal,
    tables: &'a GrammarView,
    words: usize,
    points: bool,
}

impl Drop for Walk<'_> {
    fn drop(&mut self) {
        self.scratch.stack.clear();
        self.scratch.reductions.clear();
        self.scratch.positions.clear();
        self.scratch.masks.clear();
    }
}

fn reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<(), Error> {
    values
        .try_reserve(additional)
        .map_err(|_| Error::Allocation)
}

impl Walk<'_> {
    fn aliases(&self, production: u16) -> *const u16 {
        if production == 0 {
            ptr::null()
        } else {
            unsafe {
                self.tables
                    .alias_sequences
                    .add(production as usize * self.tables.max_alias_sequence_length as usize)
            }
        }
    }

    fn fields(&self, production: u16) -> Range {
        if self.tables.production_fields.is_null() {
            Range {
                offset: 0,
                length: 0,
            }
        } else {
            unsafe { *self.tables.production_fields.add(production as usize) }
        }
    }

    fn supertype(&self, symbol: u16) -> u16 {
        if u32::from(symbol) < self.tables.symbol_count {
            unsafe { *self.tables.supertype_indexes.add(symbol as usize) }
        } else {
            0
        }
    }

    #[inline]
    fn child_mask(&mut self, mask: Mask, visible: bool, symbol: u16) -> Result<Mask, Error> {
        if self.words == 0 {
            return Ok(Mask(0));
        }
        let supertype = self.supertype(symbol);
        if self.words == 1 {
            let mut bits = if visible { 0 } else { mask.0 };
            if supertype != 0 {
                bits |= 1 << (supertype - 1);
            }
            return Ok(Mask(bits));
        }
        let masks = &mut self.scratch.masks;
        let offset = masks.len();
        reserve(masks, self.words)?;
        masks.resize(offset + self.words, 0);
        if !visible {
            masks.copy_within(mask.0 as usize..mask.0 as usize + self.words, offset);
        }
        if supertype != 0 {
            let index = usize::from(supertype - 1);
            masks[offset + index / 64] |= 1 << (index % 64);
        }
        Ok(Mask(offset as u64))
    }

    fn mask_id(&self, mask: Mask) -> Result<u16, Error> {
        if self.tables.supertype_count <= 8 {
            return Ok(mask.0 as u16);
        }
        let words = if self.words == 1 {
            std::slice::from_ref(&mask.0)
        } else {
            &self.scratch.masks[mask.0 as usize..mask.0 as usize + self.words]
        };
        // This hash and probing order match the native grammar dictionary.
        let mut hash = 14695981039346656037u64;
        for word in words {
            hash = (hash ^ word).wrapping_mul(1099511628211);
            hash ^= hash >> 32;
        }
        let limit = self.tables.supertype_table_capacity - 1;
        let mut bucket = hash as u32 & limit;
        loop {
            let entry = unsafe { *self.tables.supertype_table.add(bucket as usize) };
            if entry == 0 {
                return Err(Error::Language);
            }
            let index = entry - 1;
            let stored = unsafe {
                std::slice::from_raw_parts(
                    self.tables.supertype_masks.add(index as usize * self.words),
                    self.words,
                )
            };
            if words == stored {
                return Ok(index as u16);
            }
            bucket = (bucket + 1) & limit;
        }
    }

    fn push(
        &mut self,
        node: Node,
        subtree: Subtree,
        visible: bool,
        facts: &Facts,
    ) -> Result<(), Error> {
        let position_mark = self.scratch.positions.len();
        let mask_mark = self.scratch.masks.len();
        let mut frame = Frame {
            node,
            subtree,
            children: ptr::null(),
            aliases: ptr::null(),
            inline_position: Position::default(),
            child_end: node.position.bytes + facts.size,
            position_mark,
            position_offset: if facts.children == 1 {
                usize::MAX
            } else {
                position_mark
            },
            fields: Range {
                offset: 0,
                length: 0,
            },
            mask_mark,
            child_mask: Mask(0),
            remaining: facts.children,
            structural: 0,
            visible,
            child_later: false,
        };
        if facts.children != 0 {
            let (children, production, symbol) = subtree.branch();
            frame.children = children;
            frame.aliases = self.aliases(production);
            frame.fields = self.fields(production);
            if self.points && facts.children > 1 {
                reserve(&mut self.scratch.positions, facts.children as usize)?;
            }
            let mut position = node.position;
            for index in 0..facts.children as usize {
                let child = Subtree(unsafe { children.add(index) });
                let extra = if self.points {
                    let (padding, size, extra) = child.lengths();
                    if index != 0 {
                        position = position.add(padding);
                    }
                    if facts.children == 1 {
                        frame.inline_position = position;
                    } else {
                        self.scratch.positions.push(position);
                    }
                    position = position.add(size);
                    extra
                } else {
                    child.facts().extra
                };
                frame.structural += u32::from(!extra);
            }
            frame.child_mask = self.child_mask(
                node.mask,
                visible,
                if node.alias == 0 { symbol } else { node.alias },
            )?;
        }
        reserve(&mut self.scratch.stack, 1)?;
        self.scratch.stack.push(frame);
        Ok(())
    }

    #[inline(never)]
    fn descend_hidden(&self, subtree: &mut Subtree, node: &mut Node, facts: &mut Facts) {
        while !facts.visible && node.alias == 0 && facts.children == 1 {
            let (children, production, symbol) = subtree.branch();
            let child = Subtree(children);
            *facts = child.facts();
            let mut alias = 0;
            let mut field = None;
            if !facts.extra {
                let aliases = self.aliases(production);
                if !aliases.is_null() {
                    alias = unsafe { *aliases };
                }
                field = node.field;
                let fields = self.fields(production);
                if fields.length != 0 {
                    field = FieldId::new(unsafe {
                        *self.tables.direct_fields.add(fields.offset as usize)
                    })
                    .or(field);
                }
            }
            if self.words == 1 {
                let supertype = self.supertype(symbol);
                if supertype != 0 {
                    node.mask.0 |= 1 << (supertype - 1);
                }
            }
            *subtree = child;
            node.alias = alias;
            node.field = field;
        }
    }

    #[inline(always)]
    fn emit(&self, builder: &mut Builder, subtree: Subtree, node: &Node) -> Result<(), Error> {
        let (size, symbol, extra, missing, error) = unsafe {
            let subtree = &*subtree.0;
            if ffi::SubtreeInlineData::is_inline_raw(ptr::addr_of!(subtree.data)) {
                let data = &subtree.data;
                (
                    Position {
                        bytes: data.size_bytes.into(),
                        point: Point {
                            row: 0,
                            column: data.size_bytes.into(),
                        },
                    },
                    u16::from(data.symbol),
                    data.extra(),
                    data.is_missing(),
                    data.is_missing(),
                )
            } else {
                let heap = subtree.ptr;
                (
                    (*heap).size.into(),
                    (*heap).symbol,
                    ffi::SubtreeHeapData::extra_raw(heap),
                    ffi::SubtreeHeapData::is_missing_raw(heap),
                    ffi::SubtreeHeapData::is_missing_raw(heap) || (*heap).error_cost != 0,
                )
            }
        };
        let end = if self.points {
            node.position.add(size)
        } else {
            Position {
                bytes: node.position.bytes + size.bytes,
                point: Point::default(),
            }
        };
        self.emit_values(
            builder,
            node,
            end,
            symbol,
            (u16::from(extra) << 1) | (u16::from(missing) << 2) | (u16::from(error) << 3),
        )
    }

    // Keep node metadata in registers through column encoding.
    #[inline(always)]
    fn emit_values(
        &self,
        builder: &mut Builder,
        node: &Node,
        end: Position,
        symbol: u16,
        flags: u16,
    ) -> Result<(), Error> {
        let original = match symbol {
            u16::MAX => self.tables.symbol_count as u16,
            65534 => (self.tables.symbol_count + 1) as u16,
            _ => symbol,
        };
        let display = unsafe {
            *self.tables.public_symbols.add(if node.alias == 0 {
                original
            } else {
                node.alias
            } as usize)
        };
        builder.emit(
            &InputNode {
                start_byte: node.position.bytes,
                end_byte: end.bytes,
                start_point: if self.points {
                    node.position.point
                } else {
                    Point::default()
                },
                end_point: if self.points {
                    end.point
                } else {
                    Point::default()
                },
                symbol: RemappedKindId(display),
                grammar: RemappedGrammarKindId(original),
                field: node.field,
                supertype: self.mask_id(node.mask)?,
                flags: u16::from(!node.later) | flags,
            },
            node.boundary,
        )
    }
}

impl Walk<'_> {
    fn push_reduction(
        &mut self,
        node: Node,
        index: u32,
        reduction: &Reduction,
        visible: bool,
    ) -> Result<(), Error> {
        let mask_mark = self.scratch.masks.len();
        let child_mask = self.child_mask(
            node.mask,
            visible,
            if node.alias == 0 {
                reduction.symbol
            } else {
                node.alias
            },
        )?;
        reserve(&mut self.scratch.reductions, 1)?;
        self.scratch.reductions.push(ReductionFrame {
            node,
            index,
            next_child: reduction.first_child,
            mask_mark,
            child_mask,
            visible,
            child_later: false,
        });
        Ok(())
    }

    #[inline(always)]
    fn emit_reduction(
        &self,
        builder: &mut Builder,
        node: &Node,
        reduction: &Reduction,
    ) -> Result<(), Error> {
        self.emit_values(
            builder,
            node,
            Position {
                bytes: reduction.end_byte,
                point: reduction.end_point,
            },
            reduction.symbol,
            u16::from(reduction.extra) << 1,
        )
    }
}

pub(super) fn pack_reductions(
    builder: &mut Builder,
    tables: &GrammarView,
    scratch: &mut Traversal,
    nodes: &[Reduction],
    root: u32,
) -> Result<(), Error> {
    let mut walk = Walk {
        scratch,
        tables,
        words: tables.supertype_count.div_ceil(64) as usize,
        points: builder.points,
    };
    if walk.words > 1 {
        reserve(&mut walk.scratch.masks, walk.words)?;
        walk.scratch.masks.resize(walk.words, 0);
    }
    let reduction = &nodes[root as usize];
    let node = Node {
        position: Position {
            bytes: reduction.start_byte,
            point: reduction.start_point,
        },
        mask: Mask(0),
        boundary: builder.distance(),
        field: None,
        alias: 0,
        later: false,
    };
    walk.push_reduction(node, root, reduction, true)?;
    while let Some(frame) = walk.scratch.reductions.last_mut() {
        if frame.next_child == u32::MAX {
            let frame = walk.scratch.reductions.last().unwrap();
            if frame.visible {
                walk.emit_reduction(builder, &frame.node, &nodes[frame.index as usize])?;
            }
            walk.scratch.masks.truncate(frame.mask_mark);
            walk.scratch.reductions.pop();
            continue;
        }
        let mut index = frame.next_child;
        let mut child = &nodes[index as usize];
        frame.next_child = child.next_sibling;
        let mut field = if frame.visible || child.extra {
            None
        } else {
            frame.node.field
        };
        field = child.field.or(field);
        let later = frame.child_later || (!frame.visible && frame.node.later);
        frame.child_later = true;
        let mut mask = frame.child_mask;

        if walk.words <= 1 {
            // Hidden reductions always have a child with visible output.
            while !child.visible && nodes[child.first_child as usize].next_sibling == u32::MAX {
                if walk.words == 1 {
                    let supertype = walk.supertype(child.symbol);
                    if supertype != 0 {
                        mask.0 |= 1 << (supertype - 1);
                    }
                }
                index = child.first_child;
                child = &nodes[index as usize];
                if child.extra {
                    field = None;
                }
                field = child.field.or(field);
            }
        }
        let node = Node {
            position: Position {
                bytes: child.start_byte,
                point: child.start_point,
            },
            mask,
            boundary: builder.distance(),
            field,
            alias: child.alias,
            later,
        };
        if child.first_child == u32::MAX {
            walk.emit_reduction(builder, &node, child)?;
        } else {
            walk.push_reduction(node, index, child, child.visible)?;
        }
    }
    Ok(())
}

pub(super) fn pack(
    builder: &mut Builder,
    tables: &GrammarView,
    scratch: &mut Traversal,
    root: Root<'_>,
) -> Result<(), Error> {
    let mut walk = Walk {
        scratch,
        tables,
        words: tables.supertype_count.div_ceil(64) as usize,
        points: builder.points,
    };
    if walk.words > 1 {
        reserve(&mut walk.scratch.masks, walk.words)?;
        walk.scratch.masks.resize(walk.words, 0);
    }
    let node = Node {
        position: root.position,
        alias: root.alias,
        field: None,
        later: false,
        mask: Mask(0),
        boundary: builder.distance(),
    };
    walk.push(node, root.subtree, true, &root.subtree.facts())?;
    while let Some(frame) = walk.scratch.stack.last_mut() {
        if frame.remaining == 0 {
            let frame = walk.scratch.stack.last().unwrap();
            if frame.visible {
                walk.emit(builder, frame.subtree, &frame.node)?;
            }
            walk.scratch.positions.truncate(frame.position_mark);
            walk.scratch.masks.truncate(frame.mask_mark);
            walk.scratch.stack.pop();
            continue;
        }
        frame.remaining -= 1;
        let index = frame.remaining as usize;
        let mut subtree = Subtree(unsafe { frame.children.add(index) });
        let mut facts = subtree.facts();
        let position_byte = if walk.points {
            0
        } else {
            let start = frame.child_end - facts.size;
            frame.child_end = if index == 0 {
                start
            } else {
                start - facts.padding
            };
            start
        };
        if !facts.extra {
            frame.structural -= 1;
        }
        let alias = if facts.extra || frame.aliases.is_null() {
            0
        } else {
            unsafe { *frame.aliases.add(frame.structural as usize) }
        };
        let later = frame.child_later || (!frame.visible && frame.node.later);
        frame.child_later |= alias != 0 || facts.visible || facts.visible_children != 0;
        let mut field = if frame.visible || facts.extra {
            None
        } else {
            frame.node.field
        };
        if !facts.extra && frame.structural < frame.fields.length {
            field = FieldId::new(unsafe {
                *tables
                    .direct_fields
                    .add(frame.fields.offset as usize + frame.structural as usize)
            })
            .or(field);
        }
        if facts.children == 0 && !facts.visible && alias == 0 {
            continue;
        }
        let position = if walk.points {
            if frame.position_offset == usize::MAX {
                frame.inline_position
            } else {
                walk.scratch.positions[frame.position_offset + index]
            }
        } else {
            Position {
                bytes: position_byte,
                point: Point::default(),
            }
        };
        let mut node = Node {
            position,
            alias,
            field,
            later,
            mask: frame.child_mask,
            boundary: builder.distance(),
        };
        if facts.children == 1 && !facts.visible && alias == 0 && walk.words <= 1 {
            walk.descend_hidden(&mut subtree, &mut node, &mut facts);
        }
        if facts.children == 0 {
            if facts.visible || node.alias != 0 {
                walk.emit(builder, subtree, &node)?;
            }
        } else {
            walk.push(node, subtree, facts.visible || node.alias != 0, &facts)?;
        }
    }
    Ok(())
}
