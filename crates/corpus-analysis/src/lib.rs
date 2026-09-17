//! Corpus inventory, grammar loading, and deterministic input preparation.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tree_sitter::{Language, ParseOptions, Parser, Tree};

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A domain and key get their own deterministic stream, independent of callers.
pub fn seed_for(seed: u64, domain: &str, key: &str) -> u64 {
    let mut hash = Sha256::new();
    hash.update(seed.to_le_bytes());
    for part in [domain, key] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    u64::from_le_bytes(hash.finalize()[..8].try_into().unwrap())
}

pub struct Random(u64);
impl Random {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        // SplitMix64: all state is local to one named operation or input file.
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }
    pub fn index(&mut self, length: usize) -> usize {
        if length == 0 {
            0
        } else {
            (self.next_u64() % length as u64) as usize
        }
    }
}

pub fn mutate(source: &[u8], seed: u64, relative_path: &str) -> Vec<u8> {
    let mut random = Random::new(seed_for(seed, "mutations", relative_path));
    let mut bytes = source.to_vec();
    for _ in 0..3 {
        let start = random.index(bytes.len() + 1);
        let length = random.index((bytes.len() - start).min(64) + 1);
        match random.index(3) {
            0 => {
                bytes.drain(start..start + length);
            }
            1 => {
                let tokens: &[&[u8]] = &[b"}", b"\n", b"\"", b"/*", b"\xff", b"()", b"<>"];
                let token = tokens[random.index(tokens.len())];
                bytes.splice(start..start, token.iter().copied());
            }
            _ => {
                let moved: Vec<_> = bytes.drain(start..start + length).collect();
                let destination = random.index(bytes.len() + 1);
                bytes.splice(destination..destination, moved);
            }
        }
    }
    bytes
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Grammar {
    pub library: PathBuf,
    pub symbol: String,
    #[serde(default)]
    pub library_sha256: String,
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub queries: Vec<QuerySource>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QuerySource {
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Registry {
    pub grammars: BTreeMap<String, Grammar>,
    #[serde(default = "default_suffixes")]
    pub suffixes: BTreeMap<String, String>,
    #[serde(default)]
    pub code_corpora_sha: String,
}
fn default_suffixes() -> BTreeMap<String, String> {
    serde_json::from_str(include_str!("../extensions.json")).unwrap()
}
impl Registry {
    pub fn read(path: &Path) -> Result<Self> {
        let mut registry: Self = serde_json::from_slice(&fs::read(path)?)?;
        let parent = path.parent().unwrap_or(Path::new("."));
        for grammar in registry.grammars.values_mut() {
            if grammar.library.is_relative() {
                grammar.library = parent.join(&grammar.library);
            }
            for query in &mut grammar.queries {
                if query.path.is_relative() {
                    query.path = parent.join(&query.path);
                }
            }
        }
        Ok(registry)
    }
    /// Read the code-corpora grammar runtime image's artifact records.
    pub fn from_artifacts(root: &Path) -> Result<Self> {
        let catalog_path = root.parent().unwrap_or(root).join("grammar-catalog.json");
        let allowed = if catalog_path.is_file() {
            let rows: Vec<serde_json::Value> = serde_json::from_slice(&fs::read(catalog_path)?)?;
            Some(
                rows.into_iter()
                    .filter_map(|row| {
                        (row["status"] == "built")
                            .then(|| row["name"].as_str().map(str::to_owned))
                            .flatten()
                    })
                    .collect::<std::collections::BTreeSet<_>>(),
            )
        } else {
            None
        };
        let mut grammars = BTreeMap::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if allowed.as_ref().is_some_and(|names| !names.contains(&name)) {
                continue;
            }
            let artifact = entry.path().join("artifact.json");
            if !artifact.is_file() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_slice(&fs::read(artifact)?)?;
            let Some(symbol) = value["symbol"].as_str() else {
                continue;
            };
            let library = entry.path().join("parser.so");
            if !library.is_file() {
                continue;
            }
            grammars.insert(
                name,
                Grammar {
                    library,
                    symbol: symbol.to_owned(),
                    library_sha256: value["sha256"].as_str().unwrap_or_default().to_owned(),
                    sha: value["sha"].as_str().unwrap_or_default().to_owned(),
                    queries: Vec::new(),
                },
            );
        }
        ensure!(
            !grammars.is_empty(),
            "no grammar artifacts at {}",
            root.display()
        );
        Ok(Self {
            grammars,
            suffixes: default_suffixes(),
            code_corpora_sha: String::new(),
        })
    }
    pub fn classify(&self, path: &Path) -> Option<&str> {
        let name = path.file_name()?.to_str()?;
        self.suffixes
            .get(name)
            .or_else(|| {
                path.extension()
                    .and_then(|extension| self.suffixes.get(extension.to_str()?))
            })
            .map(String::as_str)
    }
}

/// Field drop order keeps the shared library loaded until Language is dropped.
pub struct LoadedGrammar {
    pub language: Language,
    pub sha256: String,
    _library: libloading::Library,
}
impl LoadedGrammar {
    /// Load native grammar code in the caller's grammar container.
    ///
    /// # Safety
    /// The library must export the requested Tree-sitter language function.
    /// Keep this owner alive until every derived language, parser, tree, and
    /// query has been dropped: cloning Language does not retain a native DSO.
    pub unsafe fn open(grammar: &Grammar) -> Result<Self> {
        let sha256 = digest(&fs::read(&grammar.library)?);
        ensure!(
            grammar.library_sha256.is_empty() || sha256 == grammar.library_sha256,
            "grammar checksum mismatch: {}",
            grammar.library.display()
        );
        let library = unsafe { libloading::Library::new(&grammar.library) }
            .with_context(|| format!("load {}", grammar.library.display()))?;
        let function: libloading::Symbol<
            unsafe extern "C" fn() -> *const tree_sitter::ffi::TSLanguage,
        > = unsafe { library.get(grammar.symbol.as_bytes()) }?;
        let pointer = unsafe { function() };
        ensure!(!pointer.is_null(), "grammar returned a null language");
        let language = unsafe { Language::from_raw(pointer) };
        Ok(Self {
            language,
            sha256,
            _library: library,
        })
    }
}

pub fn parse(parser: &mut Parser, source: &[u8], timeout: Duration) -> Result<Tree> {
    ensure!(
        source.len() <= u32::MAX as usize,
        "source exceeds Tree-sitter's byte range"
    );
    let started = Instant::now();
    let mut progress = |_: &tree_sitter::ParseState| {
        if started.elapsed() >= timeout {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    };
    parser
        .parse_with_options(
            &mut |offset, _| source.get(offset..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        )
        .context("parse cancelled or timed out")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Input {
    pub path: String,
    pub grammar: String,
    pub bytes: u64,
}
#[derive(Default, Debug, Serialize)]
pub struct Inventory {
    pub inputs: Vec<Input>,
    pub unclassified: usize,
    pub unavailable_grammar: usize,
    pub oversized: usize,
    pub symlinks: usize,
    pub errors: Vec<String>,
}

pub fn inventory(
    root: &Path,
    registry: &Registry,
    repositories: &[String],
    max_bytes: u64,
) -> Inventory {
    let mut result = Inventory::default();
    for split in ["train", "training", "test"] {
        let directory = root.join(split);
        if directory.is_symlink() || !directory.is_dir() {
            continue;
        }
        let paths: Vec<_> = if repositories.is_empty() {
            vec![directory]
        } else {
            repositories
                .iter()
                .map(|repository| directory.join(repository))
                .filter(|path| path.is_dir() && !path.is_symlink())
                .collect()
        };
        for path in paths {
            for entry in walkdir::WalkDir::new(path)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry| entry.file_name() != ".git")
            {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        result.errors.push(error.to_string());
                        continue;
                    }
                };
                if entry.file_type().is_symlink() {
                    result.symlinks += 1;
                    continue;
                }
                if !entry.file_type().is_file() {
                    continue;
                }
                let Some(grammar) = registry.classify(entry.path()) else {
                    result.unclassified += 1;
                    continue;
                };
                if !registry.grammars.contains_key(grammar) {
                    result.unavailable_grammar += 1;
                    continue;
                }
                let bytes = match entry.metadata() {
                    Ok(metadata) => metadata.len(),
                    Err(error) => {
                        result.errors.push(error.to_string());
                        continue;
                    }
                };
                if bytes > max_bytes {
                    result.oversized += 1;
                    continue;
                }
                let path = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if path.contains(['\n', '\r']) {
                    result.errors.push(format!("newline in path: {path:?}"));
                    continue;
                }
                result.inputs.push(Input {
                    path,
                    grammar: grammar.to_owned(),
                    bytes,
                });
            }
        }
    }
    result.inputs.sort_by(|a, b| a.path.cmp(&b.path));
    result
}

pub fn select(mut inputs: Vec<Input>, count: Option<usize>, seed: u64, domain: &str) -> Vec<Input> {
    inputs.sort_by_key(|input| (seed_for(seed, domain, &input.path), input.path.clone()));
    if let Some(count) = count {
        inputs.truncate(count);
    }
    inputs
}

pub fn read_sampling(
    root: &Path,
    samples: &Path,
    name: &str,
    registry: &Registry,
) -> Result<Vec<Input>> {
    let mut result = Vec::new();
    for line in fs::read_to_string(samples.join(name))?.lines() {
        let relative = Path::new(line);
        ensure!(
            !relative.is_absolute()
                && !relative
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)),
            "sampling path escapes corpus: {line}"
        );
        let path = root.join(relative);
        let Some(grammar) = registry.classify(&path) else {
            bail!("unclassified input: {line}");
        };
        ensure!(
            registry.grammars.contains_key(grammar),
            "unavailable grammar: {grammar}"
        );
        result.push(Input {
            path: line.to_owned(),
            grammar: grammar.to_owned(),
            bytes: fs::metadata(path)?.len(),
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streams_are_stable_and_separate() {
        assert_ne!(seed_for(1, "sampling", "x"), seed_for(1, "mutations", "x"));
        assert_ne!(seed_for(1, "a", "bc"), seed_for(1, "ab", "c"));
        let bytes = b"abcdefghijklmnopqrstuvwxyz";
        let expected = mutate(bytes, 42, "train/a.rs");
        let _unrelated = mutate(bytes, 42, "test/b.rs");
        assert_eq!(expected, mutate(bytes, 42, "train/a.rs"));
        assert_ne!(expected, mutate(bytes, 43, "train/a.rs"));
    }
    #[test]
    fn selection_does_not_depend_on_inventory_order() {
        let mut inputs: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(|path| Input {
                path: path.into(),
                grammar: "json".into(),
                bytes: 0,
            })
            .collect();
        let expected: Vec<_> = select(inputs.clone(), Some(2), 42, "tiny")
            .into_iter()
            .map(|input| input.path)
            .collect();
        inputs.reverse();
        assert_eq!(
            expected,
            select(inputs, Some(2), 42, "tiny")
                .into_iter()
                .map(|input| input.path)
                .collect::<Vec<_>>()
        );
    }
}

/// Size buckets shared by staging and structural sampling; the middle gap is intentional.
pub fn size_bucket(bytes: u64) -> Option<&'static str> {
    if bytes < 4096 {
        Some("small")
    } else if bytes <= 100 * 1024 {
        Some("normal")
    } else if bytes > 1024 * 1024 {
        Some("large")
    } else {
        None
    }
}
