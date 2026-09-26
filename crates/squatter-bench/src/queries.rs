use crate::compare::Identities;
use anyhow::{Context, Result, bail, ensure};
use corpus_analysis::{QuerySource, digest};
use serde::Serialize;
use std::{
    collections::HashSet,
    fs,
    time::{Duration, Instant},
};
use tree_sitter::{Language, QueryCursorOptions, StreamingIterator};

pub struct Queries {
    pairs: Vec<Pair>,
    pub reports: Vec<Report>,
}
struct Pair {
    mainline: tree_sitter::Query,
    squat: tree_squatter::Query,
    name: String,
}
#[derive(Serialize)]
pub struct Report {
    name: String,
    sha256: String,
    patterns: usize,
    compile_ms: f64,
    mainline_error: Option<String>,
    squat_error: Option<String>,
}
#[derive(Debug, Eq, PartialEq)]
pub struct Record {
    query: usize,
    pattern: usize,
    capture: Option<usize>,
    nodes: Vec<(u32, usize, usize)>,
}

// Provisional snapshots and duplicate events need not agree across backends.
// Every capture of a completed match must still appear in each stream.
pub fn check_capture_coverage(events: &[Record], matches: &[Record]) -> Result<()> {
    let mut seen = HashSet::new();
    for event in events {
        let index = event.capture.context("missing capture index")?;
        let node = event.nodes.get(index).context("invalid capture index")?;
        seen.insert((event.query, event.pattern, *node));
    }
    for result in matches {
        for node in &result.nodes {
            // Capture ranges exclude nodes ending at the range's start.
            if node.2 == 0 {
                continue;
            }
            ensure!(
                seen.contains(&(result.query, result.pattern, *node)),
                "missing completed capture: query {}, pattern {}, node {:?}",
                result.query,
                result.pattern,
                node
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_coverage_allows_provisional_states_but_requires_completed_captures() {
        let record = |capture, nodes| Record {
            query: 0,
            pattern: 0,
            capture,
            nodes,
        };
        let matches = vec![record(None, vec![(0, 1, 2), (1, 2, 3), (2, 0, 0)])];
        let mut events = vec![
            record(Some(0), vec![(0, 1, 2)]),
            record(Some(0), vec![(0, 1, 2)]),
            record(Some(0), vec![(1, 3, 4)]),
        ];
        assert!(check_capture_coverage(&events, &matches).is_err());
        events.push(record(Some(1), vec![(0, 1, 2), (1, 2, 3)]));
        events.reverse();
        assert!(check_capture_coverage(&events, &matches).is_ok());
        events.push(record(Some(1), vec![]));
        assert!(check_capture_coverage(&events, &matches).is_err());
    }
}
impl Queries {
    pub fn load(language: &Language, sources: &[QuerySource]) -> Result<Self> {
        ensure!(
            !sources.is_empty(),
            "no query sources in registry; use cargo xtask squat or populate grammar.queries"
        );
        let mut result = Self {
            pairs: Vec::new(),
            reports: Vec::new(),
        };
        let prepared = tree_squatter::Language::new(language)?;
        for source in sources {
            let text = fs::read_to_string(&source.path)
                .with_context(|| format!("query {}", source.name))?;
            ensure!(
                digest(text.as_bytes()) == source.sha256,
                "query checksum mismatch: {}",
                source.name
            );
            let start = Instant::now();
            let mainline = tree_sitter::Query::new(language, &text);
            let squat = tree_squatter::Query::new(&prepared, &text);
            let report = Report {
                name: source.name.clone(),
                sha256: source.sha256.clone(),
                patterns: mainline.as_ref().map_or(0, |query| query.pattern_count()),
                compile_ms: start.elapsed().as_secs_f64() * 1000.0,
                mainline_error: mainline.as_ref().err().map(ToString::to_string),
                squat_error: squat.as_ref().err().map(ToString::to_string),
            };
            match (mainline, squat) {
                (Ok(mainline), Ok(squat)) => {
                    ensure!(
                        mainline
                            .capture_names()
                            .iter()
                            .copied()
                            .eq(squat.capture_names().iter().map(String::as_str)),
                        "capture names differ: {}",
                        source.name
                    );
                    result.pairs.push(Pair {
                        mainline,
                        squat,
                        name: source.name.clone(),
                    });
                }
                (Err(_), Err(_)) => {}
                _ => bail!(
                    "query compilation differs: {}: mainline {:?}; squat {:?}",
                    source.name,
                    report.mainline_error,
                    report.squat_error
                ),
            }
            result.reports.push(report);
        }
        ensure!(
            !result.pairs.is_empty(),
            "all queries rejected for this grammar"
        );
        Ok(result)
    }
    pub fn mainline(
        &self,
        root: tree_sitter::Node<'_>,
        ids: Option<&Identities>,
        source: &[u8],
        captures: bool,
    ) -> Result<Vec<Record>> {
        let mut output = Vec::new();
        let mut total_captures = 0;
        for (query_index, pair) in self.pairs.iter().enumerate() {
            let mut cursor = tree_sitter::QueryCursor::new();
            cursor.set_match_limit(u32::MAX);
            let start = Instant::now();
            let mut cancelled = false;
            let mut progress = |_: &tree_sitter::QueryCursorState| {
                cancelled = start.elapsed() >= Duration::from_secs(30);
                if cancelled {
                    std::ops::ControlFlow::Break(())
                } else {
                    std::ops::ControlFlow::Continue(())
                }
            };
            let options = QueryCursorOptions::new().progress_callback(&mut progress);
            let mut append = |result: &tree_sitter::QueryMatch<'_, '_>, capture| -> Result<()> {
                let Some(ids) = ids else {
                    std::hint::black_box((result, capture));
                    return Ok(());
                };
                let nodes: Vec<_> = result
                    .captures()
                    .iter()
                    .map(|entry| (entry.index, ids[&(entry.node.id())], entry.node.end_byte()))
                    .collect();
                total_captures += nodes.len();
                ensure!(
                    total_captures <= 4_000_000,
                    "query snapshot budget exceeded: {}",
                    pair.name
                );
                output.push(Record {
                    query: query_index,
                    pattern: result.pattern_index,
                    capture,
                    nodes,
                });
                Ok(())
            };
            if captures {
                let mut matches =
                    cursor.captures_with_options(&pair.mainline, root, source, options);
                while let Some((result, index)) = matches.next() {
                    append(result, Some(*index))?;
                }
            } else {
                let mut matches =
                    cursor.matches_with_options(&pair.mainline, root, source, options);
                while let Some(result) = matches.next() {
                    append(result, None)?;
                }
            }
            ensure!(!cancelled, "mainline query timed out: {}", pair.name);
            ensure!(
                !cursor.did_exceed_match_limit(),
                "mainline query match limit: {}",
                pair.name
            );
        }
        Ok(output)
    }
    pub fn squat(
        &self,
        root: tree_squatter::Node<'_>,
        ids: Option<&Identities>,
        source: &[u8],
        captures: bool,
        optimized: bool,
    ) -> Result<Vec<Record>> {
        let mut output = Vec::new();
        let mut total_captures = 0;
        for (query_index, pair) in self.pairs.iter().enumerate() {
            let mut cursor = tree_squatter::QueryCursor::new();
            cursor.set_optimized(optimized);
            cursor.set_timeout(Some(Duration::from_secs(30)));
            let mut execution = cursor.execute(&pair.squat, root, source);
            loop {
                let next = if captures {
                    execution
                        .next_capture()
                        .map(|(result, index)| (result, Some(index)))
                } else {
                    execution.next_match().map(|result| (result, None))
                };
                let Some((result, capture)) = next else {
                    break;
                };
                let Some(ids) = ids else {
                    std::hint::black_box((result, capture));
                    continue;
                };
                ensure!(
                    result.pattern_index < pair.squat.pattern_count(),
                    "invalid pattern index"
                );
                if let Some(index) = capture {
                    result
                        .captures
                        .get(index)
                        .context("invalid capture index")?;
                }
                ensure!(
                    result
                        .captures
                        .iter()
                        .all(|entry| (entry.index as usize) < pair.squat.capture_names().len()),
                    "invalid capture name index"
                );
                let nodes: Vec<_> = result
                    .captures
                    .iter()
                    .map(|entry| {
                        (
                            entry.index,
                            ids[&(u32::from(entry.node.slot()) as usize)],
                            entry.node.end_byte(),
                        )
                    })
                    .collect();
                total_captures += nodes.len();
                ensure!(
                    total_captures <= 4_000_000,
                    "query snapshot budget exceeded: {}",
                    pair.name
                );
                output.push(Record {
                    query: query_index,
                    pattern: result.pattern_index,
                    capture,
                    nodes,
                });
            }
            ensure!(
                !execution.did_cancel(),
                "squat query timed out: {}",
                pair.name
            );
            ensure!(
                execution.error().is_none(),
                "squat query execution error: {:?}",
                execution.error()
            );
        }
        Ok(output)
    }
}
