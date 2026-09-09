use crate::compare::Identities;
use anyhow::{Context, Result, bail, ensure};
use corpus_analysis::{QuerySource, digest};
use serde::Serialize;
use std::{
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
    squat: tree_sitter_squatter::Query,
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
    nodes: Vec<(u32, usize)>,
}
impl Queries {
    pub fn load(language: &Language, sources: &[QuerySource]) -> Result<Self> {
        ensure!(
            !sources.is_empty(),
            "no query sources in registry; use tools/squatter/run.py or populate grammar.queries"
        );
        let mut result = Self {
            pairs: Vec::new(),
            reports: Vec::new(),
        };
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
            let squat = tree_sitter_squatter::Query::new(language, &text);
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
        ids: &Identities,
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
                let nodes: Vec<_> = result
                    .captures()
                    .iter()
                    .map(|entry| (entry.index, ids[&(entry.node.id())]))
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
        root: tree_sitter_squatter::Node<'_>,
        ids: &Identities,
        source: &[u8],
        captures: bool,
        optimized: bool,
    ) -> Result<Vec<Record>> {
        let mut output = Vec::new();
        let mut total_captures = 0;
        for (query_index, pair) in self.pairs.iter().enumerate() {
            let mut cursor = tree_sitter_squatter::QueryCursor::new();
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
                let nodes: Vec<_> = result
                    .captures
                    .iter()
                    .map(|entry| (entry.index, ids[&(entry.node.slot() as usize)]))
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
