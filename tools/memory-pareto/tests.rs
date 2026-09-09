use super::*;

fn node(start: u64) -> Record {
    [1, 1, 0, 1, 1, start, 1, 0, 0, 0, 1]
}

fn squat_configs() -> Vec<Configuration> {
    configurations(
        &serde_json::from_str(include_str!("search.json")).unwrap(),
        &[700, 700, 40, 31],
    )
    .unwrap()
}

#[test]
fn squat_trial_changes_one_axis_at_each_capacity() {
    let configs = squat_configs();
    assert_eq!(configs.len(), 40);
    let mut ids = BTreeSet::new();
    for capacity in [4, 8, 16, 32, 64] {
        let configs: Vec<_> = configs.iter().filter(|c| c.capacity == capacity).collect();
        assert_eq!(configs.len(), 8);
        for c in &configs {
            let changes = c.widths[4..].iter().filter(|&&w| w == 16).count();
            assert!(changes <= 1);
            assert!(ids.insert(configuration_id(c)));
            assert_eq!(c.widths[1], 0);
        }
    }
    let mut search: Search = serde_json::from_str(include_str!("search.json")).unwrap();
    search.capacities.push(16);
    assert!(configurations(&search, &[700, 700, 40, 31]).is_err());
}

#[test]
fn squat_end_transforms_use_original_row_span() {
    let single = [1, 1, 0, 1, 1, 100, 5, 10, 0, 20, 5];
    let multi = [1, 1, 0, 1, 1, 100, 50, 10, 2, 20, 3];
    let a = squat::transform(&single);
    let b = squat::transform(&multi);
    assert_eq!((a[6], a[8], a[10]), (105, 10, 25));
    assert_eq!((b[6], b[8], b[10]), (150, 12, 3));
}

#[test]
fn squat_grouping_matches_literal_reverse_deltas_and_soa_allocation() {
    let mut seed = 9127u64;
    let mut random = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        seed >> 32
    };
    for count in [0, 1, 4, 8, 16, 32, 33, 64, 250] {
        let records: Vec<Record> = (0..count)
            .map(|_| {
                [
                    1,
                    1,
                    0,
                    1,
                    random() % 1200,
                    random() % 900,
                    random() % 1000,
                    random() % 600,
                    random() % 20,
                    random() % 500,
                    random() % 300,
                ]
            })
            .collect();
        for c in squat_configs() {
            let mut groups: Vec<Vec<Record>> = Vec::new();
            let mut overflow_slots = 0;
            for record in &records {
                let r = squat::transform(record);
                let fits = |group: &[Record]| {
                    (4..11).all(|column| {
                        let values: Vec<_> = group
                            .iter()
                            .chain(std::iter::once(&r))
                            .map(|n| n[column])
                            .collect();
                        let reverse = [6, 8, 10].contains(&column);
                        let base = if reverse {
                            *values.iter().max().unwrap()
                        } else {
                            *values.iter().min().unwrap()
                        };
                        values
                            .iter()
                            .all(|&v| v.abs_diff(base) < (1 << c.widths[column]))
                    })
                };
                if let Some(group) = groups.last()
                    && group.len() < c.capacity as usize
                    && !fits(group)
                {
                    overflow_slots += c.capacity as usize - group.len();
                }
                if groups
                    .last()
                    .is_none_or(|g| g.len() == c.capacity as usize || !fits(g))
                {
                    groups.push(Vec::new());
                }
                groups.last_mut().unwrap().push(r);
            }
            let m = simulate(&records, &c).unwrap();
            let group_count = groups.len() as u64;
            let slots = group_count * c.capacity as u64;
            // Literal sections: 16-byte C header, u8 counts, aligned u32 bases,
            // then every field's contiguous payload (including five bitmaps).
            let mut expected_bytes = 16 + aligned(group_count, 4) + group_count * 28;
            for (column, &width) in c.widths.iter().enumerate() {
                if width == 0 || slots == 0 {
                    continue;
                }
                if column == 3 {
                    expected_bytes += 5 * slots.div_ceil(8);
                    continue;
                }
                if column >= GRAMMAR_COLUMNS && width == 16 && !expected_bytes.is_multiple_of(2) {
                    expected_bytes += 1;
                }
                if column < GRAMMAR_COLUMNS {
                    while !expected_bytes.is_multiple_of(8) {
                        expected_bytes += 1;
                    }
                    // Literally place each lane, opening another whole word
                    // only when the next value would straddle its boundary.
                    let mut used = 64;
                    for _ in 0..slots {
                        if used + u64::from(width) > 64 {
                            expected_bytes += 8;
                            used = 0;
                        }
                        used += u64::from(width);
                    }
                } else {
                    expected_bytes += (slots * u64::from(width)).div_ceil(8);
                }
            }
            assert_eq!(m.total_bytes, expected_bytes);
            assert_eq!(m.groups, group_count);
            assert_eq!(
                m.overflow_waste_bits,
                overflow_slots as u64 * c.widths.iter().map(|&w| w as u64).sum::<u64>()
            );
        }
    }
}

#[test]
fn squat_swar_waste_is_per_array_and_separate_from_empty_slots() {
    let config = &squat_configs()[2]; // 16 slots, 10-bit symbols, 6-bit fields.
    let full = simulate(&vec![node(0); 48], config).unwrap();
    assert_eq!(full.groups, 3);
    // 48 symbols: eight words, four unused bits each, no spare lanes.
    // 48 fields: five words, four unused bits each, two spare six-bit lanes.
    assert_eq!(full.lane_waste_bits, 8 * 4 + 5 * 4);
    assert_eq!(full.word_tail_waste_bits, 2 * 6);
    assert_eq!(full.padding_bits, 52 + 12 + 8); // One byte aligns the bases.
    assert_eq!(full.total_bytes, 574);
    assert_eq!(full.overflow_waste_bits + full.final_waste_bits, 0);

    let partial = simulate(&vec![node(0); 33], config).unwrap();
    assert_eq!(partial.total_bytes, full.total_bytes);
    assert_eq!(partial.padding_bits, full.padding_bits);
    assert_eq!(partial.final_waste_bits, 15 * 77);
    // Two groups have an extra four-byte alignment before the symbol array.
    let two = simulate(&vec![node(0); 32], config).unwrap();
    assert_eq!(two.lane_waste_bits, 40);
    assert_eq!(two.word_tail_waste_bits, 88);
    assert_eq!(two.padding_bits, 40 + 88 + (2 + 4) * 8);

    for config in squat_configs().iter().take(5) {
        // Same 192 physical slots at each capacity: words span group edges.
        let metrics = simulate(&vec![node(0); 192], config).unwrap();
        assert_eq!(metrics.lane_waste_bits, 32 * 4 + 20 * 4);
        assert_eq!(metrics.word_tail_waste_bits, 8 * 6);
    }
}

#[test]
fn squat_swar_handles_empty_byte_width_and_zero_width_columns() {
    let mut config = squat_configs()[2].clone();
    let empty = simulate(&[], &config).unwrap();
    assert_eq!(empty.total_bytes, 16);
    assert_eq!(empty.padding_bits, 0);
    // Byte-width lanes fill words, and absent columns add no alignment.
    config.widths[0] = 8;
    config.widths[2] = 0;
    let metrics = simulate(&vec![node(0); 16], &config).unwrap();
    assert_eq!(metrics.lane_waste_bits + metrics.word_tail_waste_bits, 0);
    config.widths[0] = 0;
    let mut zero = node(0);
    zero[0] = 0;
    let metrics = simulate(&vec![zero; 16], &config).unwrap();
    assert_eq!(metrics.total_bytes, 170);
    assert_eq!(metrics.padding_bits, 3 * 8);
}

#[test]
fn squat_small_groups_round_bitmaps_and_align_u16_arrays_once() {
    let config = squat_configs()[0].clone(); // Four slots.
    let records = vec![node(0); 4];
    let metrics = simulate(&records, &config).unwrap();
    assert_eq!(metrics.total_bytes, 97);
    assert_eq!(metrics.lane_waste_bits, 8);
    assert_eq!(metrics.word_tail_waste_bits, 56);
    // Three header alignment bytes plus four unused bits in each flag bitmap.
    assert_eq!(metrics.padding_bits, 8 + 56 + 3 * 8 + 5 * 4);
    let mut wide = config.clone();
    wide.widths[4] = 16;
    let metrics = simulate(&records, &wide).unwrap();
    assert_eq!(metrics.total_bytes, 102); // Four extra value bytes, one alignment byte.
    assert_eq!(metrics.padding_bits, 8 + 56 + 3 * 8 + 5 * 4 + 8);

    let metrics = simulate(&vec![node(0); 8], &config).unwrap();
    // Two four-slot groups share each flag byte; they do not get five bytes each.
    assert_eq!(metrics.total_bytes, 165);
    let mut eight = config;
    eight.capacity = 8;
    let metrics = simulate(&vec![node(0); 8], &eight).unwrap();
    assert_eq!(metrics.total_bytes, 133);
}

#[test]
fn pareto_preserves_ties_and_tradeoffs() {
    let first = Metrics {
        total_bytes: 100,
        group_header_bytes: 10,
        ..Metrics::default()
    };
    let worse = Metrics {
        total_bytes: 101,
        ..first.clone()
    };
    let tradeoff = Metrics {
        total_bytes: 90,
        group_header_bytes: 11,
        ..Metrics::default()
    };
    assert!(dominates(&first, &worse));
    assert!(!dominates(&first, &first));
    assert!(!dominates(&first, &tradeoff));
    assert!(!dominates(&tradeoff, &first));
}

#[test]
fn sweep_frontier_matches_pairwise_dominance() {
    let mut random = 7u64;
    let mut evaluations: Vec<Evaluation> = (0..600)
        .map(|index| {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            Evaluation {
                configuration_id: configuration_id(&squat_configs()[0].clone()),
                configuration: squat_configs()[0].clone(),
                totals: Metrics {
                    total_bytes: random % 19,
                    overflow_waste_bits: (random >> 16) % 13,
                    group_header_bytes: (random >> 32) % 7,
                    ..Metrics::default()
                },
                invalid_files: if index % 11 == 0 {
                    vec!["invalid".into()]
                } else {
                    vec![]
                },
                pareto: false,
            }
        })
        .collect();
    mark_frontier(&mut evaluations);
    for evaluation in &evaluations {
        let expected = evaluation.invalid_files.is_empty()
            && !evaluations.iter().any(|other| {
                other.invalid_files.is_empty() && dominates(&other.totals, &evaluation.totals)
            });
        assert_eq!(evaluation.pareto, expected);
    }
}

#[test]
fn search_rejects_old_axes_and_reserves_error_symbols() {
    for input in [
        r#"{"capacities":[16],"variants":[]}"#,
        r#"{"capacities":[16],"widths":{}}"#,
    ] {
        assert!(serde_json::from_str::<Search>(input).is_err());
    }
    let search = Search {
        capacities: vec![16],
    };
    let configs = configurations(&search, &[255, 255, 0, 31]).unwrap();
    assert_eq!(configs[0].widths[0], 9); // real IDs 0..254, error IDs 255 and 256
    assert_eq!(configs[0].widths[2], 2); // SWAR minimum even for field-less grammars
    for capacities in [vec![], vec![3], vec![128], vec![16, 16]] {
        assert!(configurations(&Search { capacities }, &[1, 1, 0, 31]).is_err());
    }
}
