// Prototype formats stay at version 0; no persisted data needs backward compatibility.
use std::path::{Component, Path, PathBuf};
use tree_squatter::{Language, LanguageHash, language_hash, representation_id};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LanguageVersion {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
}

/// Language identity and the hash used to validate cached tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageIdentity {
    pub name: String,
    pub version: Option<LanguageVersion>,
    pub hash: LanguageHash,
}

impl LanguageIdentity {
    pub fn new(tree_sitter_language: &tree_sitter::Language, fallback_name: &str) -> Self {
        Self::with_fallback_version(tree_sitter_language, fallback_name, None)
    }

    pub fn new_with_version(
        tree_sitter_language: &tree_sitter::Language,
        fallback_name: &str,
        fallback_version: LanguageVersion,
    ) -> Self {
        Self::with_fallback_version(tree_sitter_language, fallback_name, Some(fallback_version))
    }

    fn with_fallback_version(
        tree_sitter_language: &tree_sitter::Language,
        fallback_name: &str,
        fallback_version: Option<LanguageVersion>,
    ) -> Self {
        let version = tree_sitter_language
            .metadata()
            .map(|metadata| LanguageVersion {
                major: metadata.major_version,
                minor: metadata.minor_version,
                patch: metadata.patch_version,
            });
        Self {
            name: tree_sitter_language
                .name()
                .unwrap_or(fallback_name)
                .to_owned(),
            version: version.or(fallback_version),
            hash: language_hash(
                tree_sitter_language,
                fallback_name,
                fallback_version.map(|version| [version.major, version.minor, version.patch]),
            ),
        }
    }
}

#[derive(Clone)]
pub struct IdentifiedLanguage {
    pub(crate) prepared: Language,
    pub(crate) identity: LanguageIdentity,
}

impl IdentifiedLanguage {
    /// Pair a prepared language with its identity.
    /// Use `Persistence::prepare_language` to restore persisted tables.
    pub fn new(prepared: Language, identity: LanguageIdentity) -> Self {
        Self { prepared, identity }
    }

    pub fn identity(&self) -> &LanguageIdentity {
        &self.identity
    }
}

pub(crate) fn path(path: &Path) -> Result<(PathBuf, Vec<u8>), crate::LoadError> {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => clean.push(name),
            Component::CurDir => (),
            _ => return Err(crate::LoadError::InvalidPath),
        }
    }
    if clean.as_os_str().is_empty() || clean.starts_with(".tree-sitter") {
        return Err(crate::LoadError::InvalidPath);
    }
    let mut encoded = vec![if cfg!(windows) { 1 } else { 0 }];
    encoded.extend_from_slice(clean.as_os_str().as_encoded_bytes());
    Ok((clean, encoded))
}

pub(crate) fn digest(domain: &str, bytes: &[u8]) -> [u8; 32] {
    blake3::derive_key(domain, bytes)
}

pub(crate) fn representation() -> [u8; 32] {
    let bytes = representation_id().raw().to_le_bytes();
    digest("tree-squatter representation v0", &bytes)
}

pub(crate) fn runtime() -> [u8; 32] {
    let hex = env!("TSQ_RUNTIME_FINGERPRINT").as_bytes();
    let nibble = |c: u8| if c <= b'9' { c - b'0' } else { c - b'a' + 10 };
    std::array::from_fn(|i| nibble(hex[2 * i]) * 16 + nibble(hex[2 * i + 1]))
}

pub(crate) fn language_key(hash: LanguageHash) -> [u8; 40] {
    let mut key = [0; 40];
    key[..8].copy_from_slice(&hash.raw().to_le_bytes());
    key[8..].copy_from_slice(&runtime());
    key
}

pub(crate) struct SourceIdentity {
    pub path: Vec<u8>,
    pub key: [u8; 72],
    pub current_guard: CurrentGuard,
}

impl SourceIdentity {
    pub fn new(path: Vec<u8>, source: &[u8]) -> Self {
        let mut key = [0; 72];
        key[..32].copy_from_slice(&digest("tree-squatter path v0", &path));
        key[32..40].copy_from_slice(&(source.len() as u64).to_le_bytes());
        key[40..].copy_from_slice(blake3::hash(source).as_bytes());
        Self {
            path,
            key,
            current_guard: CurrentGuard::Unchecked,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Request {
    pub path: Vec<u8>,
    pub source_key: [u8; 72],
    pub tree_key: [u8; 104],
    pub header: Vec<u8>,
    pub presence: bool,
    pub points: bool,
    pub current_guard: CurrentGuard,
}

#[derive(Clone)]
pub(crate) enum CurrentGuard {
    Unchecked,
    Missing,
    Retired([u8; 8]),
    Current([u8; 72]),
}

impl Request {
    pub fn new(
        path: Vec<u8>,
        source: &[u8],
        language: &IdentifiedLanguage,
        presence: bool,
        points: bool,
    ) -> Self {
        Self::from_source(
            &SourceIdentity::new(path, source),
            language,
            presence,
            points,
        )
    }

    pub fn from_source(
        source: &SourceIdentity,
        language: &IdentifiedLanguage,
        presence: bool,
        points: bool,
    ) -> Self {
        let source_key = source.key;
        let mut identity = Vec::new();
        identity.extend_from_slice(&language.identity.hash.raw().to_le_bytes());
        identity.extend_from_slice(&runtime());
        identity.extend_from_slice(&representation());
        // Keep the envelope aligned for transaction-backed core slabs.
        identity.extend_from_slice(&u64::from(points).to_le_bytes());
        let variant = digest("tree-squatter cache variant v0", &identity);
        let mut tree_key = [0; 104];
        tree_key[..72].copy_from_slice(&source_key);
        tree_key[72..].copy_from_slice(&variant);
        let mut header = b"TSQENT00".to_vec();
        header.extend_from_slice(&source_key[32..]);
        header.extend_from_slice(&identity);
        header.extend_from_slice(&variant);
        Self {
            path: source.path.clone(),
            source_key,
            tree_key,
            header,
            presence,
            points,
            current_guard: source.current_guard.clone(),
        }
    }

    #[cfg(test)]
    pub fn encode(&self, slab: &[u8]) -> Vec<u8> {
        let mut value = self.header.clone();
        value.extend_from_slice(&(slab.len() as u64).to_le_bytes());
        value.extend_from_slice(slab);
        value
    }

    pub fn decode<'a>(&self, value: &'a [u8]) -> Option<&'a [u8]> {
        let tail = value.strip_prefix(self.header.as_slice())?;
        let length = u64::from_le_bytes(tail.get(..8)?.try_into().ok()?);
        let slab = tail.get(8..)?;
        (usize::try_from(length).ok()? == slab.len()).then_some(slab)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_identity_covers_old_and_new_abi_tables() {
        let json = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast())
        };
        let c_sharp = unsafe {
            tree_sitter::Language::from_raw(tree_sitter_c_sharp::LANGUAGE.into_raw()().cast())
        };
        let old = LanguageIdentity::new(&json, "json");
        let new = LanguageIdentity::new(&c_sharp, "c_sharp");
        assert_eq!(old.name, "json");
        assert_eq!(old.version, None);
        assert_eq!(Some(new.name.as_str()), c_sharp.name());
        assert!(new.version.is_some());
        assert_eq!(old, LanguageIdentity::new(&json, "json"));
        assert_ne!(old.hash, LanguageIdentity::new(&json, "other_json").hash);
        assert_ne!(old.hash, new.hash);

        let fallback_version = LanguageVersion {
            major: 1,
            minor: 2,
            patch: 3,
        };
        let versioned = LanguageIdentity::new_with_version(&json, "json", fallback_version);
        assert_eq!(versioned.version, Some(fallback_version));
        assert_ne!(versioned.hash, old.hash);
        assert_ne!(
            versioned.hash,
            LanguageIdentity::new_with_version(
                &json,
                "json",
                LanguageVersion {
                    patch: 4,
                    ..fallback_version
                }
            )
            .hash
        );
        assert_eq!(
            new,
            LanguageIdentity::new_with_version(&c_sharp, "ignored", fallback_version)
        );
    }

    #[test]
    fn paths_are_unambiguous() {
        for invalid in [
            "",
            ".",
            "../a",
            "a/../b",
            "/a",
            ".tree-sitter/a",
            ".tree-sitter/big-endian/a",
        ] {
            assert!(path(Path::new(invalid)).is_err(), "{invalid}");
        }
        assert_eq!(
            path(Path::new("./a/b")).unwrap(),
            path(Path::new("a/b")).unwrap()
        );
        assert_ne!(
            path(Path::new("a")).unwrap(),
            path(Path::new("a/.source")).unwrap()
        );
    }

    #[test]
    fn envelope_rejects_truncation_length_overflow_and_trailing_data() {
        let request = Request {
            path: vec![],
            source_key: [0; 72],
            tree_key: [0; 104],
            header: vec![7; 192],
            presence: true,
            points: true,
            current_guard: CurrentGuard::Unchecked,
        };
        let encoded = request.encode(b"slab");
        assert_eq!(&encoded[192..200], &4u64.to_le_bytes());
        assert_eq!(request.decode(&encoded), Some(b"slab".as_slice()));
        for end in 0..encoded.len() {
            assert!(request.decode(&encoded[..end]).is_none());
        }
        let mut bad = encoded.clone();
        bad[192..200].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(request.decode(&bad).is_none());
        let mut bad = encoded.clone();
        bad.push(0);
        assert!(request.decode(&bad).is_none());
        let mut bad = encoded;
        bad[0] ^= 1;
        assert!(request.decode(&bad).is_none());
    }
}
