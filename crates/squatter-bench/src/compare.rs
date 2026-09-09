use anyhow::{Result, ensure};
use std::collections::HashMap;
use tree_sitter::{Language, Point};
use tree_sitter_squatter::traits::{Attributes, CursorLike, NodeLike};

pub type Identities = HashMap<usize, usize>;

pub fn identities<'tree, N: NodeLike<'tree>>(root: N) -> Result<Identities> {
    let mut cursor = root.cursor()?;
    let mut result = HashMap::new();
    loop {
        let ordinal = result.len();
        ensure!(
            result.insert(cursor.node().identity(), ordinal).is_none(),
            "duplicate mainline node identity"
        );
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(result);
            }
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Record<'tree> {
    pub ordinal: usize,
    pub attributes: Attributes<'tree>,
    pub field: Option<u16>,
    pub depth: u32,
}

fn reverse_walk<'tree, N: NodeLike<'tree>>(
    root: N,
    ids: &Identities,
) -> Result<Vec<Record<'tree>>> {
    struct Frame<N> {
        node: N,
        field: Option<u16>,
        children: Vec<(N, Option<u16>)>,
    }
    fn frame<'tree, N: NodeLike<'tree>>(node: N, field: Option<u16>) -> Result<Frame<N>> {
        let mut cursor = node.cursor()?;
        let mut children = Vec::new();
        if cursor.goto_first_child() {
            loop {
                children.push((cursor.node(), cursor.field_id()));
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        Ok(Frame {
            node,
            field,
            children,
        })
    }
    // Mainline's reverse cursor can lose structural child indexes (and thus
    // fields/aliases). Enumerate siblings forward once, then consume them in
    // reverse. Both backends use this same adapter and pay the same cache cost.
    let mut stack = vec![frame(root, None)?];
    let mut records = Vec::with_capacity(ids.len());
    while let Some(current) = stack.last_mut() {
        if let Some((child, field)) = current.children.pop() {
            stack.push(frame(child, field)?);
        } else {
            let current = stack.pop().unwrap();
            records.push(Record {
                ordinal: ids[&current.node.identity()],
                attributes: current.node.attributes(),
                field: current.field,
                depth: stack.len() as u32,
            });
        }
    }
    Ok(records)
}

pub fn walk<'tree, N: NodeLike<'tree>>(
    root: N,
    ids: &Identities,
    backward: bool,
) -> Result<Vec<Record<'tree>>> {
    if backward {
        return reverse_walk(root, ids);
    }
    let mut cursor = root.cursor()?;
    let mut records = Vec::with_capacity(ids.len());
    loop {
        let node = cursor.node();
        records.push(Record {
            ordinal: ids[&node.identity()],
            attributes: node.attributes(),
            field: cursor.field_id(),
            depth: cursor.depth(),
        });
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(records);
            }
        }
    }
}

pub fn seek_bytes<'tree, N: NodeLike<'tree>>(
    root: N,
    ids: &Identities,
    positions: &[usize],
) -> Vec<Option<usize>> {
    positions
        .iter()
        .map(|&position| {
            root.descendant_for_byte_range(position, position)
                .map(|node| ids[&node.identity()])
        })
        .collect()
}
pub fn seek_points<'tree, N: NodeLike<'tree>>(
    root: N,
    ids: &Identities,
    positions: &[Point],
) -> Vec<Option<usize>> {
    positions
        .iter()
        .map(|&position| {
            root.descendant_for_point_range(position, position)
                .map(|node| ids[&node.identity()])
        })
        .collect()
}

/// Untimed relationship checks complement the timed attribute walks. Full checks
/// on small trees and evenly spaced checks on large trees avoid quadratic test
/// setup from repeatedly finding mainline parents from the root.
pub fn relationships<'tree, A: NodeLike<'tree>, B: NodeLike<'tree>>(
    mainline: A,
    squat: B,
    mainline_ids: &Identities,
    squat_ids: &Identities,
    language: &Language,
) -> Result<()> {
    let mut first = mainline.cursor()?;
    let mut second = squat.cursor()?;
    let stride = (mainline_ids.len() / 1000).max(1);
    let identity_a = |node: Option<A>| node.map(|node| mainline_ids[&node.identity()]);
    let identity_b = |node: Option<B>| node.map(|node| squat_ids[&node.identity()]);
    loop {
        let a = first.node();
        let b = second.node();
        let ordinal = mainline_ids[&a.identity()];
        if ordinal % stride == 0 {
            let a_relations = [
                a.parent(),
                a.next_sibling(),
                a.prev_sibling(),
                a.next_named_sibling(),
                a.prev_named_sibling(),
            ];
            let b_relations = [
                b.parent(),
                b.next_sibling(),
                b.prev_sibling(),
                b.next_named_sibling(),
                b.prev_named_sibling(),
            ];
            for (index, (a, b)) in a_relations.into_iter().zip(b_relations).enumerate() {
                ensure!(
                    identity_a(a) == identity_b(b),
                    "relationship {index} differs at ordinal {ordinal}"
                );
            }
            let attributes = a.attributes();
            for index in 0..=attributes.child_count {
                ensure!(
                    identity_a(a.child(index)) == identity_b(b.child(index)),
                    "child {index} differs at ordinal {ordinal}"
                );
            }
            for index in 0..=attributes.named_child_count {
                ensure!(
                    identity_a(a.named_child(index)) == identity_b(b.named_child(index)),
                    "named child {index} differs at ordinal {ordinal}"
                );
            }
            for field in 1..=language.field_count() {
                ensure!(
                    identity_a(a.child_by_field_id(field as u16))
                        == identity_b(b.child_by_field_id(field as u16)),
                    "field {field} differs at ordinal {ordinal}"
                );
            }
        }
        let down = first.goto_first_child();
        ensure!(
            down == second.goto_first_child(),
            "cursor child transition differs at {ordinal}"
        );
        if down {
            continue;
        }
        loop {
            let next = first.goto_next_sibling();
            ensure!(
                next == second.goto_next_sibling(),
                "cursor sibling transition differs at {ordinal}"
            );
            if next {
                break;
            }
            let parent = first.goto_parent();
            ensure!(
                parent == second.goto_parent(),
                "cursor parent transition differs at {ordinal}"
            );
            if !parent {
                return Ok(());
            }
        }
    }
}
