//! Separately linked measurements for construction, loading, and destruction.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use corpus_analysis::{LoadedGrammar, Registry, digest, digest_file};
use std::{fs, hint::black_box, mem::MaybeUninit, path::PathBuf, sync::Arc, time::Instant};
use tree_squatter::{Language, PackContext, PackOptions, Query, StableSlab, Tree};

#[derive(Parser, serde::Serialize)]
struct Arguments {
    #[arg(long)]
    registry: PathBuf,
    inputs: Vec<PathBuf>,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 7)]
    samples: usize,
    #[arg(long, default_value_t = 30)]
    sample_ms: u64,
    /// Fix operation counts for separately instrumented allocation profiles.
    #[arg(long)]
    iterations: Option<usize>,
    #[arg(long)]
    workload: Vec<String>,
    /// Registry query name; otherwise use the first supported query per grammar.
    #[arg(long)]
    query: Option<String>,
    #[arg(long)]
    no_points: bool,
    #[arg(long)]
    no_presence: bool,
    #[arg(long)]
    repack: bool,
}

struct Slab(Arc<Tree>);

// Trees publish immutable, aligned slabs whose address survives Arc moves.
unsafe impl StableSlab for Slab {
    fn bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

struct Case<'input> {
    tree_sitter_language: &'input tree_sitter::Language,
    native: &'input tree_sitter::Tree,
    query: &'input str,
    capture: Option<String>,
    language: Language,
    cache: Vec<u8>,
    tree: Arc<Tree>,
    packer: PackContext,
    compact: Vec<MaybeUninit<u8>>,
    options: PackOptions,
}

const WORKLOADS: &[&str] = &[
    "pack-cold",
    "pack-reuse",
    "pack-trim",
    "point-access",
    "load-full",
    "load-safety",
    "load-borrowed",
    "load-backed",
    "compact-copy",
    "repack",
    "language-new",
    "language-cache",
    "query-new",
    "query-drop",
    "query-disable-pattern",
    "query-disable-capture",
];

impl Case<'_> {
    fn run(&mut self, workload: &str, iterations: usize) -> f64 {
        // Dispatch once, outside the repeated operation. Returned owners are
        // dropped inside the timing except where destruction is measured alone.
        macro_rules! measure {
            ($operation:expr) => {{
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box($operation);
                }
                start.elapsed().as_secs_f64()
            }};
        }
        match workload {
            "pack-cold" => {
                measure!(
                    Tree::pack_with_options(&self.language, self.native, self.options).unwrap()
                )
            }
            "pack-reuse" => measure!(
                self.packer
                    .pack_with_options(&self.language, self.native, self.options)
                    .unwrap()
            ),
            "pack-trim" => measure!({
                self.packer.trim();
                self.packer
                    .pack_with_options(&self.language, self.native, self.options)
                    .unwrap()
            }),
            "point-access" => measure!({
                for node in self.tree.root_node().preorder() {
                    black_box((node.start_position(), node.end_position()));
                }
            }),
            "load-full" => {
                measure!(Tree::from_bytes(&self.language, self.tree.as_bytes()).unwrap())
            }
            "load-safety" => measure!(
                Tree::from_bytes_safety_checked(&self.language, self.tree.as_bytes()).unwrap()
            ),
            "load-borrowed" => {
                measure!(Tree::from_bytes_borrowed(&self.language, self.tree.as_bytes()).unwrap())
            }
            "load-backed" => {
                measure!(Tree::from_owned_slab(&self.language, Slab(self.tree.clone())).unwrap())
            }
            "compact-copy" => measure!(self.tree.copy_compact_into(&mut self.compact).unwrap()),
            "repack" => measure!(self.tree.repack().unwrap()),
            "language-new" => measure!(Language::new(self.tree_sitter_language).unwrap()),
            "language-cache" => {
                measure!(Language::from_cache(self.tree_sitter_language, &self.cache).unwrap())
            }
            "query-new" => measure!(Query::new(&self.language, self.query).unwrap()),
            "query-drop" | "query-disable-pattern" | "query-disable-capture" => {
                let mut elapsed = 0.0;
                let mut remaining = iterations;
                // Keep setup out of the clock without retaining an unbounded
                // number of programs during the destruction/mutation samples.
                let mut queries = Vec::with_capacity(16);
                while remaining != 0 {
                    let count = remaining.min(16);
                    queries.extend(
                        (0..count).map(|_| Query::new(&self.language, self.query).unwrap()),
                    );
                    let start = Instant::now();
                    match workload {
                        "query-drop" => queries.clear(),
                        "query-disable-pattern" => {
                            for query in &mut queries {
                                query.disable_pattern(black_box(tree_squatter::PatternIx(0)));
                            }
                        }
                        _ => {
                            for query in &mut queries {
                                query.disable_capture(black_box(self.capture.as_deref().unwrap()));
                            }
                        }
                    }
                    elapsed += start.elapsed().as_secs_f64();
                    queries.clear();
                    remaining -= count;
                }
                elapsed
            }
            _ => unreachable!(),
        }
    }
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    ensure!(!arguments.inputs.is_empty(), "provide source files");
    ensure!(
        arguments.samples > 0 && arguments.sample_ms > 0,
        "samples must be positive"
    );
    ensure!(
        arguments.iterations != Some(0),
        "iterations must be positive"
    );
    ensure!(!arguments.output.exists(), "output already exists");
    for workload in &arguments.workload {
        ensure!(
            WORKLOADS.contains(&workload.as_str()),
            "unknown workload: {workload}"
        );
    }
    let registry = Registry::read(&arguments.registry)?;
    let binary_sha256 = digest_file(std::env::current_exe()?)?;
    let mut rows = Vec::new();

    for path in &arguments.inputs {
        let name = registry.classify(path).context("unclassified input")?;
        let grammar = registry.grammars.get(name).context("missing grammar")?;
        let loaded = unsafe { LoadedGrammar::open(grammar)? };
        let source = fs::read(path)?;
        let language = &loaded.language;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(language)?;
        let native = parser.parse(&source, None).context("parse failed")?;
        let owner = Language::new(language)?;
        let options = PackOptions {
            points: !arguments.no_points,
            symbol_presence: !arguments.no_presence,
            repack: arguments.repack,
            ..Default::default()
        };
        let tree = Arc::new(Tree::pack_with_options(&owner, &native, options)?);
        let selected = grammar.queries.iter().find_map(|query| {
            if arguments
                .query
                .as_ref()
                .is_some_and(|name| name != &query.name)
            {
                return None;
            }
            let text = fs::read_to_string(&query.path).ok()?;
            Query::new(&owner, &text)
                .ok()
                .map(|compiled| (query, text, compiled))
        });
        let (query_source, query_text, compiled) =
            selected.context("no supported query selected")?;
        ensure!(
            digest(query_text.as_bytes()) == query_source.sha256,
            "query checksum differs"
        );
        let capture = compiled
            .capture_names()
            .first()
            .map(|name| (*name).to_owned());
        let patterns = compiled.pattern_count();
        drop(compiled);

        let mut case = Case {
            tree_sitter_language: language,
            native: &native,
            query: &query_text,
            capture,
            cache: owner.cache()?,
            language: owner,
            compact: vec![MaybeUninit::uninit(); tree.compact_size()],
            tree,
            packer: PackContext::new()?,
            options,
        };
        for &workload in WORKLOADS {
            if !arguments.workload.is_empty()
                && !arguments.workload.iter().any(|name| name == workload)
            {
                continue;
            }
            if workload == "query-disable-capture" && case.capture.is_none() {
                continue;
            }
            if workload == "query-disable-pattern" && patterns == 0 {
                continue;
            }

            let setup_start = Instant::now();
            let warm = case.run(workload, 1);
            let limit = if workload == "query-drop" || workload.starts_with("query-disable-") {
                // Compilation is outside these clocks but can dominate probe
                // runtime. Bound that setup to about 100ms per sample.
                (0.1 / setup_start.elapsed().as_secs_f64())
                    .ceil()
                    .clamp(1.0, 1024.0) as usize
            } else {
                10000
            };
            let iterations = arguments.iterations.unwrap_or_else(|| {
                ((arguments.sample_ms as f64 / 1000.0 / warm.max(1e-9)).ceil() as usize)
                    .clamp(1, limit)
            });
            let seconds: Vec<_> = (0..arguments.samples)
                .map(|_| case.run(workload, iterations))
                .collect();
            rows.push(serde_json::json!({
                "path": path, "grammar": name, "source_sha256": digest(&source),
                "grammar_sha256": loaded.sha256, "query": query_source.name,
                "query_sha256": query_source.sha256, "workload": workload,
                "iterations": iterations, "seconds": seconds,
                "source_bytes": source.len(), "slab_bytes": case.tree.as_bytes().len(),
                "nodes": case.tree.root_node().descendant_count(),
                "compact_bytes": case.tree.compact_size(), "language_cache_bytes": case.cache.len(),
            }));
            eprintln!("{} / {workload}", path.display());
        }
    }
    fs::write(
        &arguments.output,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": 1, "arguments": arguments, "results": rows,
            "backend": squatter_bench::BACKEND, "binary_sha256": binary_sha256,
            "resident": "one core, source, mainline tree/parser, grammar, slab, reusable packer and compact destination; query mutations retain up to 16 compiled programs",
            "timing_contract": "complete operation and destruction; query-drop and query-disable exclude compilation; load-backed includes Arc clone and owner allocation; compact-copy reuses destination; point-access traverses all nodes and reads both endpoints without source lookup",
        }))?,
    )?;
    Ok(())
}
