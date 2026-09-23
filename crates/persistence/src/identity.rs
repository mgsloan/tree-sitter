// Prototype formats stay at version 0; no persisted data needs backward compatibility.
use std::path::{Component, Path, PathBuf};

/// Digest of the actual grammar implementation, supplied by its provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrammarFingerprint(pub [u8; 32]);

#[derive(Clone)]
pub struct Grammar {
    pub(crate) prepared: tree_sitter_squatter::Grammar,
    pub(crate) fingerprint: GrammarFingerprint,
}

impl Grammar {
    /// Pair a prepared grammar with its implementation fingerprint.
    /// Use `Persistence::prepare_grammar` to restore persisted tables.
    pub fn new(prepared: tree_sitter_squatter::Grammar, fingerprint: GrammarFingerprint) -> Self {
        Self {
            prepared,
            fingerprint,
        }
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
    if clean.as_os_str().is_empty() || clean.starts_with(".tree-squatter") {
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
    let bytes = tree_sitter_squatter::representation_id().to_le_bytes();
    digest("tree-squatter representation v0", &bytes)
}

pub(crate) fn runtime() -> [u8; 32] {
    let hex = env!("TSQ_RUNTIME_FINGERPRINT").as_bytes();
    let nibble = |c: u8| if c <= b'9' { c - b'0' } else { c - b'a' + 10 };
    std::array::from_fn(|i| nibble(hex[2 * i]) * 16 + nibble(hex[2 * i + 1]))
}

pub(crate) fn grammar_key(fingerprint: GrammarFingerprint) -> [u8; 64] {
    let mut key = [0; 64];
    key[..32].copy_from_slice(&fingerprint.0);
    key[32..].copy_from_slice(&runtime());
    key
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
        grammar: &Grammar,
        presence: bool,
        points: bool,
    ) -> Self {
        let path_id = digest("tree-squatter path v0", &path);
        let mut source_key = [0; 72];
        source_key[..32].copy_from_slice(&path_id);
        source_key[32..40].copy_from_slice(&(source.len() as u64).to_le_bytes());
        source_key[40..].copy_from_slice(blake3::hash(source).as_bytes());
        let mut identity = Vec::new();
        identity.extend_from_slice(&grammar.fingerprint.0);
        identity.extend_from_slice(&runtime());
        identity.extend_from_slice(&representation());
        let variant = digest("tree-squatter cache variant v0", &identity);
        let mut tree_key = [0; 104];
        tree_key[..72].copy_from_slice(&source_key);
        tree_key[72..].copy_from_slice(&variant);
        let mut header = b"TSQENT00".to_vec();
        header.extend_from_slice(&source_key[32..]);
        header.extend_from_slice(&identity);
        header.extend_from_slice(&variant);
        Self {
            path,
            source_key,
            tree_key,
            header,
            presence,
            points,
            current_guard: CurrentGuard::Unchecked,
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
    fn paths_are_unambiguous() {
        for invalid in [
            "",
            ".",
            "../a",
            "a/../b",
            "/a",
            ".tree-squatter/a",
            ".tree-squatter/big-endian/a",
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
