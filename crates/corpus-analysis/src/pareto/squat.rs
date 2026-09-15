//! Storage model for the repository-root design, using schema-2 input trees.
use super::*;

/// Lay out each complete field array once, after the number of groups is known.
/// Physical slot IDs include overflow-abandoned and final-group reserved slots.
pub(super) fn apply_layout(metrics: &mut Metrics, configuration: &Configuration) {
    let slots = metrics.groups * u64::from(configuration.capacity);
    metrics.file_header_bytes = 16;
    metrics.group_header_bytes = metrics.groups * 29;
    metrics.lane_waste_bits = 0;
    metrics.word_tail_waste_bits = 0;
    // u8 counts followed by seven u32 base arrays.
    let mut cursor = 16 + aligned(metrics.groups, 4) + metrics.groups * 28;
    for (column, width) in configuration.widths.into_iter().enumerate() {
        if width == 0 || slots == 0 {
            continue;
        }
        if column == 3 {
            // Five separate flag bitmaps, each rounded only at its array end.
            // Four-slot groups can leave four spare bits in each bitmap.
            cursor += 5 * slots.div_ceil(8);
            continue;
        }
        let width = u64::from(width);
        if column >= GRAMMAR_COLUMNS {
            cursor = aligned(cursor, (width / 8) as u32);
        }
        let bytes = if column < GRAMMAR_COLUMNS {
            cursor = aligned(cursor, 8);
            let lanes = 64 / width;
            let words = slots.div_ceil(lanes);
            metrics.lane_waste_bits += words * (64 % width);
            metrics.word_tail_waste_bits += (words * lanes - slots) * width;
            words * 8
        } else {
            slots * width / 8
        };
        cursor += bytes;
    }
    // u8/u16 arrays align once at their start, with no word rounding or padding
    // at group boundaries. Remaining padding includes bitmap byte rounding.
    let reserved_bits = slots
        * configuration
            .widths
            .iter()
            .map(|&w| u64::from(w))
            .sum::<u64>();
    metrics.padding_bits =
        (cursor - metrics.file_header_bytes - metrics.group_header_bytes) * 8 - reserved_bits;
    metrics.total_bytes = cursor;
}

pub(super) fn transform(record: &Record) -> Record {
    let mut result = *record;
    // Natural grammar symbols are absent from SquatNode. Five flag bitmaps
    // have fixed storage regardless of their values; public-tree extraction
    // does not yet supply the proposed last-child/hidden flag semantics.
    result[1] = 0;
    for column in [6, 8, 10] {
        result[column] = if column != 10 || record[8] == 0 {
            record[column - 1] + record[column]
        } else {
            record[column]
        };
    }
    // Group-max minus end and end minus group-min have identical fit ranges.
    // Simulating extrema of absolute ends gives the exact reverse-delta groups.
    result
}
