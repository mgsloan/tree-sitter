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

pub fn walk<'tree, N: NodeLike<'tree>>(root: N, ids: &Identities) -> Result<Vec<Record<'tree>>> {
    let mut cursor = root.cursor()?;
    let mut records = Vec::with_capacity(ids.len());
    loop {
        let node = cursor.node();
        records.push(Record {
            ordinal: ids[&node.identity()],
            attributes: cursor.attributes(),
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

/// Native cursor movement without attribute decoding.
/// Recording every identity keeps correctness checks stronger than a checksum.
pub fn navigate<'tree, C: CursorLike<'tree>>(mut cursor: C, ids: &Identities) -> Vec<usize> {
    let mut nodes = Vec::with_capacity(ids.len());
    loop {
        nodes.push(ids[&cursor.node().identity()]);
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return nodes;
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

/// Mainline's field lookup can cross an alias-visible boundary even though its
/// cursor assigns no field there. Require the packed result to match mainline's
/// visible children before classifying such a difference as expected.
fn visible_child_by_field<'tree, N: NodeLike<'tree>>(parent: N, field: u16) -> Result<Option<N>> {
    if field == 0 || parent.attributes().is_error {
        return Ok(None);
    }
    let mut cursor = parent.cursor()?;
    if cursor.goto_first_child() {
        loop {
            if cursor.field_id() == Some(field) {
                return Ok(Some(cursor.node()));
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    Ok(None)
}

fn expected_field_mismatch(
    lookup: Option<usize>,
    visible_child: Option<usize>,
    packed: Option<usize>,
) -> bool {
    lookup != visible_child && packed == visible_child
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
    expected_fields: &mut usize,
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
        if ordinal.is_multiple_of(stride) {
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
            // Indexed child access scans siblings. A wide array must not turn
            // the validation harness into quadratic work; cursor transitions
            // below still check every child, and small parents are exhaustive.
            let child_stride = (attributes.child_count / 100).max(1);
            for index in (0..attributes.child_count)
                .step_by(child_stride)
                .chain(std::iter::once(attributes.child_count))
            {
                ensure!(
                    identity_a(a.child(index)) == identity_b(b.child(index)),
                    "child {index} differs at ordinal {ordinal}"
                );
            }
            let named_stride = (attributes.named_child_count / 100).max(1);
            for index in (0..attributes.named_child_count)
                .step_by(named_stride)
                .chain(std::iter::once(attributes.named_child_count))
            {
                ensure!(
                    identity_a(a.named_child(index)) == identity_b(b.named_child(index)),
                    "named child {index} differs at ordinal {ordinal}"
                );
            }
            for field in 1..=language.field_count() {
                let lookup = identity_a(a.child_by_field_id(field as u16));
                let packed = identity_b(b.child_by_field_id(field as u16));
                if lookup != packed {
                    let visible_child = identity_a(visible_child_by_field(a, field as u16)?);
                    ensure!(
                        expected_field_mismatch(lookup, visible_child, packed),
                        "unexpected field {field} mismatch at ordinal {ordinal}: \
                         lookup {lookup:?}, visible child {visible_child:?}, packed {packed:?}"
                    );
                    *expected_fields += 1;
                }
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

#[cfg(test)]
mod tests {
    use super::expected_field_mismatch;

    #[test]
    fn field_policy_requires_independent_visible_child_agreement() {
        // A transitive lookup can return a grandchild with no direct field.
        assert!(expected_field_mismatch(Some(12), None, None));
        // The API can also suppress or redirect a visible child's field.
        assert!(expected_field_mismatch(None, Some(7), Some(7)));
        assert!(expected_field_mismatch(Some(12), Some(7), Some(7)));
        // A packed error remains an error, including on an exceptional parent.
        assert!(!expected_field_mismatch(Some(12), None, Some(13)));
        assert!(!expected_field_mismatch(Some(12), Some(7), None));
        assert!(!expected_field_mismatch(Some(7), Some(7), None));
        assert!(!expected_field_mismatch(None, None, Some(7)));
        assert!(!expected_field_mismatch(Some(7), Some(7), Some(7)));
    }
}
