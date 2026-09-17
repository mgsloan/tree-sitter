use super::TestLevel;
use anyhow::{Context, Result, ensure};
use clap::Args;
use corpus_analysis::{Grammar, Input, QuerySource, Registry, digest, inventory, seed_for};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};
use walkdir::WalkDir;

#[derive(Args)]
pub struct Options {
    /// Fresh directory for staged inputs, logs, and results.
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long, default_value = "../../code-corpora")]
    code_corpora: PathBuf,
    #[arg(long, default_value = "tools/squatter/matrix.toml")]
    matrix: PathBuf,
    /// Cached Podman build image; defaults to the corpus image lock.
    #[arg(long)]
    image: Option<String>,
    #[arg(long)]
    repo: Vec<String>,
    #[arg(long)]
    grammar: Vec<String>,
    #[arg(long, default_value_t = 42)]
    seed: u64,
    /// Files per split/language/size bucket (bench: 4, checks: 1).
    #[arg(long)]
    per_bucket: Option<usize>,
    #[arg(long, default_value_t = 4 * 1024 * 1024)]
    max_file_bytes: u64,
    #[arg(long, default_value_t = 3)]
    repeat: usize,
    #[arg(long, default_value_t = 1)]
    traversal_iterations: usize,
    #[arg(long, default_value_t = 1200)]
    timeout: u64,
    #[arg(long)]
    benchmark: Vec<String>,
    #[arg(long)]
    skip_mutated: bool,
    /// Also measure the layout variants in the matrix.
    #[arg(long)]
    layouts: bool,
    #[arg(long)]
    unoptimized_query: bool,
    #[arg(long)]
    pressure_profile: Vec<String>,
    #[arg(long)]
    pressure_bytes: Option<usize>,
    #[arg(long)]
    benchmark_cpu: Option<usize>,
    #[arg(long)]
    pressure_cpu: Option<usize>,
}

fn text(value: &Value) -> Result<&str> {
    value.as_str().context("expected a string in configuration")
}
fn strings(value: &Value) -> Result<Vec<String>> {
    value
        .as_array()
        .context("expected a list in configuration")?
        .iter()
        .map(|value| Ok(text(value)?.to_owned()))
        .collect()
}
fn toml(path: &Path) -> Result<Value> {
    toml_edit::de::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("read {}", path.display()))
}
fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("write {}", path.display()))
}
fn git(root: &Path, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()?;
    ensure!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
fn copy(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination.parent().context("missing parent")?)?;
    fs::copy(source, destination).with_context(|| format!("copy {}", source.display()))?;
    Ok(())
}

fn select(inputs: Vec<Input>, seed: u64, per_bucket: usize) -> Vec<Input> {
    let mut buckets: BTreeMap<_, Vec<Input>> = BTreeMap::new();
    for input in inputs {
        let Some(bucket) = corpus_analysis::size_bucket(input.bytes) else {
            continue;
        };
        let split = input.path.split('/').next().unwrap_or_default().to_owned();
        buckets
            .entry((split, input.grammar.clone(), bucket))
            .or_default()
            .push(input);
    }
    buckets
        .into_values()
        .flat_map(|mut entries| {
            entries
                .sort_by_key(|input| (seed_for(seed, "staging", &input.path), input.path.clone()));
            entries.truncate(per_bucket);
            entries
        })
        .collect()
}

struct Run {
    output: PathBuf,
    image: String,
    timeout: u64,
    manifest: Value,
}
impl Run {
    fn save(&self) -> Result<()> {
        write_json(&self.output.join("container-run.json"), &self.manifest)
    }
    fn container(&self, program: &str) -> Command {
        let mut command = Command::new("podman");
        command
            .args([
                "run",
                "--rm",
                "--network=none",
                "--cap-drop=all",
                "--security-opt=no-new-privileges",
                "--memory=8g",
                "--cpus=4",
                "--pids-limit=256",
                "--timeout",
                &self.timeout.to_string(),
                "--env",
                "ASAN_OPTIONS=detect_leaks=1",
                "--entrypoint",
                program,
            ])
            .args([
                "-v",
                &format!("{}:/work:ro", self.output.join("source").display()),
                "-v",
                &format!("{}:/out:rw", self.output.display()),
                "-e",
                &format!(
                    "SQUAT_TOOL_SHA={}",
                    self.manifest["tool_sha"].as_str().unwrap()
                ),
                "-e",
                &format!(
                    "SQUAT_SOURCE_SHA256={}",
                    self.manifest["source_sha256"].as_str().unwrap()
                ),
            ]);
        command
    }
    fn execute(&mut self, name: &str, command: &mut Command) -> Result<()> {
        let log = self.output.join(format!("{name}.log"));
        eprintln!("running {name}; log: {}", log.display());
        let output = fs::File::create(&log)?;
        command
            .stdout(Stdio::from(output.try_clone()?))
            .stderr(Stdio::from(output));
        let started = Instant::now();
        let status = command.status();
        let passed = status.as_ref().is_ok_and(std::process::ExitStatus::success);
        self.manifest["operations"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":name,
            "status":if passed { "passed".to_owned() } else { format!("{status:?}") },
            "seconds":started.elapsed().as_secs_f64(), "command":format!("{command:?}")}));
        self.save()?;
        ensure!(passed, "{name} failed; see {}", log.display());
        Ok(())
    }
    fn shell(&mut self, name: &str, script: &str, arguments: &[String]) -> Result<()> {
        let mut command = self.container("sh");
        command
            .arg(&self.image)
            .args(["-c", script, "sh"])
            .args(arguments);
        self.execute(name, &mut command)
    }
}

pub fn run(options: Options, test: Option<TestLevel>) -> Result<()> {
    ensure!(
        options.repeat > 0 && options.traversal_iterations > 0 && options.timeout > 0,
        "counts must be positive"
    );
    let per_bucket = options
        .per_bucket
        .unwrap_or(if test.is_some() { 1 } else { 4 });
    ensure!(per_bucket > 0, "--per-bucket must be positive");
    let root = crate::root_dir();
    let corpus = options.code_corpora.canonicalize()?;
    let matrix_bytes = fs::read(&options.matrix)?;
    let matrix = toml(&options.matrix)?;
    let profiles = if options.pressure_profile.is_empty() {
        vec!["isolated".to_owned()]
    } else {
        options.pressure_profile.clone()
    };
    for profile in &profiles {
        ensure!(
            matrix["pressure"].get(profile).is_some(),
            "unknown pressure profile: {profile}"
        );
    }
    ensure!(
        test.is_none() || profiles == ["isolated"],
        "checks only support isolated pressure"
    );
    let names = if options.grammar.is_empty() {
        strings(&matrix["selection"]["grammars"])?
    } else {
        options.grammar.clone()
    };
    let repositories = if options.repo.is_empty() {
        strings(&matrix["selection"]["repositories"])?
    } else {
        options.repo.clone()
    };
    let catalog = toml(&corpus.join("selected-grammars.toml"))?;
    let entries: BTreeMap<_, _> = catalog["repo"]
        .as_array()
        .context("missing grammar catalog")?
        .iter()
        .map(|entry| Ok((text(&entry["name"])?.to_owned(), entry)))
        .collect::<Result<_>>()?;
    let image = match options.image {
        Some(ref image) => image.clone(),
        None => {
            text(&toml(&corpus.join("containers/images.lock.toml"))?["build"]["local_image_id"])?
                .to_owned()
        }
    };
    ensure!(
        Command::new("podman")
            .args(["image", "exists", &image])
            .status()?
            .success(),
        "build image is not cached; use --image"
    );
    let output = options.output.as_ref().context("--output is required")?;
    ensure!(!output.exists(), "output exists: {}", output.display());
    fs::create_dir_all(output)?;
    let output = output.canonicalize()?;
    fs::write(output.join("matrix.toml"), &matrix_bytes)?;
    let mut registry: Registry = serde_json::from_value(json!({"grammars":{}}))?;
    registry.code_corpora_sha = git(&corpus, &["rev-parse", "HEAD"])?;
    for name in &names {
        let entry = entries
            .get(name)
            .with_context(|| format!("unknown grammar: {name}"))?;
        registry.grammars.insert(
            name.clone(),
            Grammar {
                library: format!("/out/grammars/{name}.so").into(),
                symbol: format!(
                    "tree_sitter_{}",
                    entry["grammar"].as_str().unwrap_or(name).replace('-', "_")
                ),
                sha: text(&entry["sha"])?.into(),
                library_sha256: String::new(),
                queries: Vec::new(),
            },
        );
    }
    let mut inventory = inventory(&corpus, &registry, &repositories, options.max_file_bytes);
    ensure!(
        inventory.errors.is_empty(),
        "corpus inventory errors: {:?}",
        inventory.errors
    );
    let candidates = inventory.inputs.len();
    let inputs = select(
        std::mem::take(&mut inventory.inputs),
        options.seed,
        per_bucket,
    );
    let mut coverage = serde_json::to_value(&inventory)?;
    coverage["candidates"] = candidates.into();
    ensure!(!inputs.is_empty(), "no files selected");
    let mut staged = Vec::new();
    for input in inputs {
        let destination = output.join("corpus").join(&input.path);
        copy(&corpus.join(&input.path), &destination)?;
        staged.push(json!({"path":input.path,"grammar":input.grammar,"bytes":input.bytes,"sha256":digest(&fs::read(destination)?)}));
    }
    for (grammar, filename) in [
        ("bash", "hidden-seek.sh"),
        ("css", "hidden-seek.css"),
        ("typescript", "inherited-field.ts"),
        ("tsx", "inherited-field.ts"),
    ] {
        if !names.iter().any(|name| name == grammar) {
            continue;
        }
        let filename = if grammar == "tsx" {
            "inherited-field.tsx"
        } else {
            filename
        };
        let path = format!("test/fixtures/{grammar}/{filename}");
        let source = root
            .join("lib/squat/tests/fixtures")
            .join(if grammar == "tsx" {
                "inherited-field.ts"
            } else {
                filename
            });
        copy(&source, &output.join("corpus").join(&path))?;
        staged.push(json!({"path":path,"grammar":grammar,"bytes":fs::metadata(&source)?.len(),"sha256":digest(&fs::read(source)?)}));
    }
    // Each run compiles its snapshot; later edits cannot change the measured C sources.
    let paths = Command::new("git")
        .current_dir(&root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "Cargo.toml",
            "Cargo.lock",
            "LICENSE",
            ".cargo",
            "lib",
            "crates",
            "tools/squatter",
        ])
        .output()?;
    ensure!(paths.status.success(), "list source snapshot");
    let mut hashes = BTreeMap::new();
    for name in paths
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::str::from_utf8(name)?;
        let source = root.join(name);
        if !source.is_file() || source.is_symlink() {
            continue;
        }
        let destination = output.join("source").join(name);
        copy(&source, &destination)?;
        hashes.insert(name.to_owned(), digest(&fs::read(destination)?));
    }
    let mut run = Run {
        output,
        image: image.clone(),
        timeout: options.timeout,
        manifest: json!({"schema":3,"partial":true,"image":image,"tool_sha":git(&root,&["rev-parse","HEAD"])?,
            "tool_dirty":!git(&root,&["status","--porcelain"])?.is_empty(),"source_sha256":digest(&serde_json::to_vec(&hashes)?),
            "matrix_sha256":digest(&matrix_bytes),"code_corpora_sha":registry.code_corpora_sha,"inputs":staged,
            "coverage":coverage,"repositories":repositories,"seed":options.seed,"selection":"seed_for(staging), per split/grammar/size",
            "missing_repositories":repositories.iter().filter(|name| !["train","training","test"].iter().any(|split|corpus.join(split).join(name).is_dir())).collect::<Vec<_>>(),
            "operations":[],"grammars":{}}),
    };
    run.save()?;
    let sanitize = test == Some(TestLevel::Sanitize);
    let flags = if sanitize {
        "-O1 -g -fsanitize=address,undefined -fno-omit-frame-pointer"
    } else {
        "-O2 -g"
    };
    fs::create_dir_all(run.output.join("grammars"))?;
    fs::create_dir_all(run.output.join("queries"))?;
    let mut query_sources: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
    if !sanitize {
        for directory in [corpus.join("zed"), corpus.join("zed-extensions")] {
            for entry in WalkDir::new(directory)
                .into_iter()
                .filter_entry(|entry| {
                    ![".git", "node_modules", "target"]
                        .contains(&entry.file_name().to_str().unwrap_or_default())
                })
                .filter_map(Result::ok)
            {
                if entry.file_name() != "config.toml" {
                    continue;
                }
                let config = toml(entry.path())?;
                if let Some(name) = config["grammar"]
                    .as_str()
                    .filter(|name| names.iter().any(|selected| selected == name))
                {
                    for file in fs::read_dir(entry.path().parent().unwrap())? {
                        let path = file?.path();
                        if path.extension().is_some_and(|extension| extension == "scm") {
                            query_sources
                                .entry(name.to_owned())
                                .or_default()
                                .insert(path);
                        }
                    }
                }
            }
        }
    }
    for (name, grammar) in &mut registry.grammars {
        let entry = entries[name];
        let checkout = corpus.join("grammars").join(name);
        let directory = entry["directory"].as_str().unwrap_or_default();
        let mut command = run.container("sh");
        command.args(["-v", &format!("{}:/grammar:ro", checkout.display())]).arg(&run.image)
            .args(["-c", "set -eu; source=$1; name=$2; flags=$3; set -- \"$source/parser.c\"; if test -f \"$source/scanner.c\"; then set -- \"$@\" \"$source/scanner.c\"; fi; cc -shared -fPIC $flags -I\"$source\" \"$@\" -o \"/out/grammars/$name.so\"", "sh",
                &format!("/grammar/{directory}/src"), name, flags]);
        run.execute(&format!("grammar-{name}"), &mut command)?;
        grammar.library_sha256 = digest(&fs::read(
            run.output.join("grammars").join(format!("{name}.so")),
        )?);
        for directory in [
            checkout.join("queries"),
            checkout.join(directory).join("queries"),
        ] {
            for file in WalkDir::new(directory).into_iter().filter_map(Result::ok) {
                if file.file_type().is_file()
                    && file
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "scm")
                {
                    query_sources
                        .entry(name.clone())
                        .or_default()
                        .insert(file.into_path());
                }
            }
        }
        for (index, path) in query_sources
            .remove(name)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let filename = format!("{name}-{index}.scm");
            let destination = run.output.join("queries").join(&filename);
            copy(path, &destination)?;
            grammar.queries.push(QuerySource {
                name: path.strip_prefix(&corpus)?.to_string_lossy().into_owned(),
                path: format!("/out/queries/{filename}").into(),
                sha256: digest(&fs::read(destination)?),
            });
        }
        run.manifest["grammars"][name] = json!({"pin":grammar.sha,"checkout_sha":git(&checkout,&["rev-parse","HEAD"])?,
            "parser_sha256":digest(&fs::read(checkout.join(directory).join("src/parser.c"))?),"library_sha256":grammar.library_sha256});
    }
    write_json(&run.output.join("registry.json"), &registry)?;
    run.save()?;
    if test.is_some() {
        run.shell("build-native", "make -C /work/lib/squat -j4 BUILD=/out/native CFLAGS=\"$1\" all check /out/native/query-check /out/native/seek-check /out/native/context-check", &[flags.into()])?;
        for (name, grammar) in &registry.grammars {
            let mut files: Vec<_> = staged
                .iter()
                .filter(|input| input["grammar"] == *name)
                .map(|input| format!("/out/corpus/{}", input["path"].as_str().unwrap()))
                .collect();
            if files.is_empty() {
                continue;
            }
            fs::write(
                run.output.join(format!("{name}-sources")),
                files.join("\n") + "\n",
            )?;
            let mut arguments = vec![
                grammar.library.to_string_lossy().into_owned(),
                grammar.symbol.clone(),
            ];
            arguments.append(&mut files);
            run.shell(&format!("native-{name}"), "set -eu; /out/native/compare \"$@\"; /out/native/context-check \"$@\"; /out/native/query-check \"$1\" \"$2\"", &arguments)?;
            run.shell(
                &format!("seek-{name}"),
                "/out/native/seek-check \"$@\"",
                &[
                    arguments[0].clone(),
                    arguments[1].clone(),
                    format!("/out/{name}-sources"),
                ],
            )?;
        }
    }
    if !sanitize {
        let mut build = Command::new("cargo");
        build
            .current_dir(run.output.join("source"))
            .args([
                "build",
                "--release",
                "--locked",
                "-p",
                "squatter-bench",
                "--target-dir",
            ])
            .arg(run.output.join("target"));
        run.execute("build-rust", &mut build)?;
        for profile in profiles {
            for mutated in [false, true] {
                if mutated && options.skip_mutated {
                    continue;
                }
                let label = format!(
                    "{}{}-{profile}",
                    if test.is_some() { "check-" } else { "" },
                    if mutated { "mutated" } else { "original" }
                );
                let mut arguments = vec![
                    format!(
                        "/out/target/release/{}",
                        if test.is_some() {
                            "squatter-check"
                        } else {
                            "squatter-bench"
                        }
                    ),
                    "--code-corpora".into(),
                    "/out/corpus".into(),
                    "--registry".into(),
                    "/out/registry.json".into(),
                    "--all".into(),
                    "--output-directory".into(),
                    "/out/bench-outputs".into(),
                    "--output".into(),
                    label.clone(),
                    "--repeat".into(),
                    options.repeat.to_string(),
                    "--seed".into(),
                    options.seed.to_string(),
                    "--traversal-iterations".into(),
                    options.traversal_iterations.to_string(),
                    "--pressure".into(),
                    text(&matrix["pressure"][&profile]["mode"])?.into(),
                    "--pressure-label".into(),
                    profile.clone(),
                ];
                for (flag, value) in [
                    ("--pressure-bytes", options.pressure_bytes),
                    ("--benchmark-cpu", options.benchmark_cpu),
                    ("--pressure-cpu", options.pressure_cpu),
                    (
                        "--pressure-duty-percent",
                        matrix["pressure"][&profile]["duty_percent"]
                            .as_u64()
                            .map(|value| value as usize),
                    ),
                ] {
                    if let Some(value) = value {
                        arguments.extend([flag.into(), value.to_string()]);
                    }
                }
                if mutated {
                    arguments.push("--mutate".into());
                }
                if options.unoptimized_query {
                    arguments.push("--unoptimized-query".into());
                }
                arguments.extend(options.benchmark.clone());
                run.shell(&label, "/lib64/ld-linux-x86-64.so.2 \"$@\"", &arguments)?;
            }
        }
        if options.layouts {
            for layout in matrix["layout"].as_array().context("missing layouts")? {
                let name = text(&layout["name"])?;
                let directory = format!("/out/layout-{name}");
                let flags = format!(
                    "-O3 -g -DSQ_GROUP_SIZE={} -DSQ_COLUMN_ALIGNMENT={}",
                    layout["group_size"], layout["alignment"]
                );
                run.shell(&format!("build-layout-{name}"), "make -C /work/lib/squat -j4 BUILD=\"$1\" CFLAGS=\"$2\" \"$1/layout-bench\" \"$1/compare\" check", &[directory.clone(),flags])?;
                for (grammar, entry) in &registry.grammars {
                    let files: Vec<_> = staged
                        .iter()
                        .filter(|input| input["grammar"] == *grammar)
                        .collect();
                    if files.is_empty() {
                        continue;
                    }
                    let mut arguments = vec![
                        entry.library.to_string_lossy().into_owned(),
                        entry.symbol.clone(),
                    ];
                    arguments.extend(
                        files.iter().map(|input| {
                            format!("/out/corpus/{}", input["path"].as_str().unwrap())
                        }),
                    );
                    run.shell(
                        &format!("layout-{name}-{grammar}"),
                        "program=$1; shift; \"$program\" \"$@\"",
                        &[vec![format!("{directory}/layout-bench")], arguments.clone()].concat(),
                    )?;
                    let mut arguments = arguments[..2].to_vec();
                    arguments.extend(
                        files
                            .iter()
                            .filter(|input| input["bytes"].as_u64().unwrap() < 4096)
                            .map(|input| {
                                format!("/out/corpus/{}", input["path"].as_str().unwrap())
                            }),
                    );
                    run.shell(
                        &format!("check-layout-{name}-{grammar}"),
                        "program=$1; shift; \"$program\" \"$@\"",
                        &[vec![format!("{directory}/compare")], arguments].concat(),
                    )?;
                }
            }
        }
    }
    run.manifest["partial"] = false.into();
    run.save()?;
    eprintln!(
        "completed {}; {} staged files",
        run.output.display(),
        staged.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_preserves_language_split_and_size_coverage() {
        let inputs: Vec<_> = ["train", "test"]
            .into_iter()
            .flat_map(|split| {
                ["json", "python"].into_iter().flat_map(move |grammar| {
                    [10, 5000, 200000, 2000000]
                        .into_iter()
                        .flat_map(move |bytes| {
                            (0..3).map(move |index| Input {
                                path: format!("{split}/{grammar}/{bytes}-{index}"),
                                grammar: grammar.into(),
                                bytes,
                            })
                        })
                })
            })
            .collect();
        let expected = select(inputs.clone(), 42, 1);
        assert_eq!(expected.len(), 12);
        let mut reversed = inputs;
        reversed.reverse();
        assert_eq!(
            expected.iter().map(|input| &input.path).collect::<Vec<_>>(),
            select(reversed, 42, 1)
                .iter()
                .map(|input| &input.path)
                .collect::<Vec<_>>()
        );
    }
}
