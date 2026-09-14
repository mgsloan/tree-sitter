//! Time real query files, including binding text predicates and result consumption.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use corpus_analysis::{Grammar, LoadedGrammar, QuerySource, digest};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    hint::black_box,
    path::PathBuf,
    time::{Duration, Instant},
};
use tree_sitter::{QueryCursorOptions, StreamingIterator};

#[derive(Parser)]
struct Args {
    job: PathBuf,
    #[arg(long, default_value_t = 5)]
    repeat: usize,
    #[arg(long)]
    skip_validation: bool,
    #[arg(long)]
    time_mainline: bool,
}
#[derive(Deserialize)]
struct Source {
    path: PathBuf,
    sha256: String,
}
#[derive(Deserialize)]
struct Job {
    name: String,
    kind: String,
    grammar: Grammar,
    query: QuerySource,
    sources: Vec<Source>,
}
struct Input {
    bytes: Vec<u8>,
    mainline: tree_sitter::Tree,
    squat: tree_sitter_squatter::Tree,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
struct Counts {
    events: u64,
    captures: u64,
    hash: u64,
}
impl Default for Counts {
    fn default() -> Self {
        Self {
            events: 0,
            captures: 0,
            hash: 14695981039346656037,
        }
    }
}
fn consume(
    pattern: usize,
    entries: impl Iterator<Item = (u32, usize, usize)>,
    counts: &mut Counts,
    records: &mut Option<Vec<Vec<u64>>>,
) -> Result<()> {
    let mut record = records.as_ref().map(|_| Vec::new());
    let mut feed = |value: u64| {
        counts.hash = (counts.hash ^ value).wrapping_mul(1099511628211);
        if let Some(r) = &mut record {
            r.push(value);
        }
    };
    feed(u64::MAX);
    feed(pattern as u64);
    for (index, start, end) in entries {
        feed(index as u64);
        feed(start as u64);
        feed(end as u64);
        counts.captures += 1;
    }
    counts.events += 1;
    ensure!(counts.captures <= 4_000_000, "capture budget exceeded");
    if let Some(records) = records {
        records.push(record.unwrap());
    }
    Ok(())
}
fn mainline(
    inputs: &[Input],
    query: &tree_sitter::Query,
    captures: bool,
    records: &mut Option<Vec<Vec<u64>>>,
) -> Result<Counts> {
    let mut counts = Counts::default();
    for input in inputs {
        let mut cursor = tree_sitter::QueryCursor::new();
        cursor.set_match_limit(u32::MAX);
        let start = Instant::now();
        let mut cancelled = false;
        let mut progress = |_: &tree_sitter::QueryCursorState| {
            cancelled = start.elapsed() > Duration::from_secs(30);
            if cancelled {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        };
        let options = QueryCursorOptions::new().progress_callback(&mut progress);
        if captures {
            let mut stream = cursor.captures_with_options(
                query,
                input.mainline.root_node(),
                input.bytes.as_slice(),
                options,
            );
            while let Some((m, index)) = stream.next() {
                let c = m.captures()[*index];
                consume(
                    m.pattern_index,
                    std::iter::once((c.index, c.node.start_byte(), c.node.end_byte())),
                    &mut counts,
                    records,
                )?;
            }
        } else {
            let mut stream = cursor.matches_with_options(
                query,
                input.mainline.root_node(),
                input.bytes.as_slice(),
                options,
            );
            while let Some(m) = stream.next() {
                consume(
                    m.pattern_index,
                    m.captures()
                        .iter()
                        .map(|c| (c.index, c.node.start_byte(), c.node.end_byte())),
                    &mut counts,
                    records,
                )?;
            }
        }
        ensure!(
            !cancelled && !cursor.did_exceed_match_limit(),
            "mainline query cancelled or match limit exceeded"
        );
    }
    Ok(counts)
}
fn squat(
    inputs: &[Input],
    query: &tree_sitter_squatter::Query,
    captures: bool,
    records: &mut Option<Vec<Vec<u64>>>,
) -> Result<Counts> {
    let mut counts = Counts::default();
    for input in inputs {
        let mut cursor = tree_sitter_squatter::QueryCursor::new();
        cursor.set_match_limit(u32::MAX);
        cursor.set_timeout(Some(Duration::from_secs(30)));
        let mut stream = cursor.execute(query, input.squat.root_node(), &input.bytes);
        if captures {
            while let Some((m, index)) = stream.next_capture() {
                let c = &m.captures[index];
                consume(
                    m.pattern_index,
                    std::iter::once((c.index, c.node.start_byte(), c.node.end_byte())),
                    &mut counts,
                    records,
                )?;
            }
        } else {
            while let Some(m) = stream.next_match() {
                consume(
                    m.pattern_index,
                    m.captures
                        .iter()
                        .map(|c| (c.index, c.node.start_byte(), c.node.end_byte())),
                    &mut counts,
                    records,
                )?;
            }
        }
        ensure!(
            stream.error().is_none(),
            "squat query error: {:?}",
            stream.error()
        );
        drop(stream);
        ensure!(
            !cursor.did_exceed_match_limit(),
            "squat match limit exceeded"
        );
    }
    Ok(counts)
}
fn cpu_seconds() -> f64 {
    let mut t = std::mem::MaybeUninit::<libc::timespec>::uninit();
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, t.as_mut_ptr()) },
        0
    );
    let t = unsafe { t.assume_init() };
    t.tv_sec as f64 + t.tv_nsec as f64 * 1e-9
}
#[derive(Serialize)]
struct Timing {
    loops: usize,
    counts: Counts,
    cpu_ms: Vec<f64>,
}
fn measure(mut operation: impl FnMut() -> Result<Counts>, repeat: usize) -> Result<Timing> {
    let expected = operation()?;
    let mut loops = 1;
    loop {
        let start = cpu_seconds();
        for _ in 0..loops {
            black_box(operation()?);
        }
        if cpu_seconds() - start >= 0.03 || loops >= 65536 {
            break;
        }
        loops *= 2;
    }
    let mut samples = Vec::new();
    for _ in 0..repeat {
        let start = cpu_seconds();
        for _ in 0..loops {
            ensure!(
                black_box(operation()?) == expected,
                "timing checksum changed"
            );
        }
        samples.push((cpu_seconds() - start) * 1000.0 / loops as f64);
    }
    Ok(Timing {
        loops,
        counts: expected,
        cpu_ms: samples,
    })
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(args.repeat > 0, "repeat must be positive");
    let job: Job = serde_json::from_slice(&fs::read(args.job)?)?;
    ensure!(
        job.kind == "highlights" || job.kind == "tags",
        "unknown query kind"
    );
    let captures = job.kind == "highlights";
    let language = unsafe { LoadedGrammar::open(&job.grammar)? };
    let text = fs::read_to_string(&job.query.path)?;
    ensure!(
        digest(text.as_bytes()) == job.query.sha256,
        "query hash mismatch"
    );
    let ts_query =
        tree_sitter::Query::new(&language.language, &text).context("mainline query compilation")?;
    let sq_query = tree_sitter_squatter::Query::new(&language.language, &text)
        .context("squat query compilation")?;
    ensure!(
        ts_query
            .capture_names()
            .iter()
            .copied()
            .eq(sq_query.capture_names().iter().map(String::as_str)),
        "capture names differ"
    );
    let general: Vec<_> = (0..sq_query.pattern_count())
        .flat_map(|p| {
            sq_query
                .general_predicates(p)
                .iter()
                .map(|q| q.operator.to_string())
        })
        .collect();
    ensure!(
        general
            .iter()
            .all(|name| ["strip!", "set-adjacent!", "select-adjacent!"].contains(&name.as_str())),
        "unsupported host predicate: {:?}",
        general
    );
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language.language)?;
    let mut inputs = Vec::new();
    for source in &job.sources {
        let bytes = fs::read(&source.path)?;
        ensure!(digest(&bytes) == source.sha256, "source hash mismatch");
        let mainline = corpus_analysis::parse(&mut parser, &bytes, Duration::from_secs(30))?;
        let squat = tree_sitter_squatter::Tree::pack(&mainline)?;
        inputs.push(Input {
            bytes,
            mainline,
            squat,
        });
    }
    if !args.skip_validation {
        let mut a = Some(Vec::new());
        let mut b = Some(Vec::new());
        let ac = mainline(&inputs, &ts_query, captures, &mut a)?;
        let bc = squat(&inputs, &sq_query, captures, &mut b)?;
        ensure!(
            ac == bc && a == b,
            "ordered query results differ for {}: mainline {:?}; squat {:?}",
            job.name,
            ac,
            bc
        );
    }
    let mainline_timing = if args.time_mainline {
        Some(measure(
            || mainline(&inputs, &ts_query, captures, &mut None),
            args.repeat,
        )?)
    } else {
        None
    };
    let squat_timing = measure(
        || squat(&inputs, &sq_query, captures, &mut None),
        args.repeat,
    )?;
    if let Some(t) = &mainline_timing {
        ensure!(t.counts == squat_timing.counts, "backend checksums differ");
    }
    println!(
        "{}",
        serde_json::json!({"job": job.name, "kind": job.kind, "points": cfg!(feature="points"),
        "validated": !args.skip_validation, "patterns": sq_query.pattern_count(), "host_directives": general,
        "files": inputs.len(), "source_bytes": inputs.iter().map(|i| i.bytes.len()).sum::<usize>(),
        "nodes": inputs.iter().map(|i| i.mainline.root_node().descendant_count()).sum::<usize>(),
        "slab_bytes": inputs.iter().map(|i| i.squat.as_bytes().len()).sum::<usize>(),
        "mainline": mainline_timing, "squat": squat_timing})
    );
    Ok(())
}
