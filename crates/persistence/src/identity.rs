use std::path::{Component, Path, PathBuf};

/// Digest of the actual grammar implementation, supplied by its provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrammarFingerprint(pub [u8; 32]);

#[derive(Clone)]
pub struct Grammar {
    pub(crate) language: tree_sitter::Language,
    pub(crate) fingerprint: GrammarFingerprint,
}

impl Grammar {
    /// The provider must pair the language with its implementation's fingerprint.
    pub fn new(language: tree_sitter::Language, fingerprint: GrammarFingerprint) -> Self {
        Self {
            language,
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
    let mut bytes = tree_sitter_squatter::representation_id()
        .to_le_bytes()
        .to_vec();
    bytes.push(u8::from(cfg!(target_endian = "big")));
    digest("tree-squatter representation v1", &bytes)
}

pub(crate) fn runtime() -> [u8; 32] {
    let hex = env!("TSQ_RUNTIME_FINGERPRINT").as_bytes();
    let nibble = |c: u8| if c <= b'9' { c - b'0' } else { c - b'a' + 10 };
    std::array::from_fn(|i| nibble(hex[2 * i]) * 16 + nibble(hex[2 * i + 1]))
}

#[derive(Clone)]
pub(crate) struct Request {
    pub path: Vec<u8>,
    pub source_key: [u8; 72],
    pub tree_key: [u8; 104],
    pub header: Vec<u8>,
}

impl Request {
    pub fn new(path: Vec<u8>, source: &[u8], grammar: &Grammar, presence: bool) -> Self {
        let path_id = digest("tree-squatter path v1", &path);
        let mut source_key = [0; 72];
        source_key[..32].copy_from_slice(&path_id);
        source_key[32..40].copy_from_slice(&(source.len() as u64).to_le_bytes());
        source_key[40..].copy_from_slice(blake3::hash(source).as_bytes());
        let mut identity = Vec::new();
        identity.extend_from_slice(&grammar.fingerprint.0);
        identity.extend_from_slice(&runtime());
        identity.extend_from_slice(&representation());
        // Compact whole-file/raw-byte mode; transient capacity is not identity.
        identity.extend_from_slice(&u64::from(presence).to_le_bytes());
        let variant = digest("tree-squatter cache variant v1", &identity);
        let mut tree_key = [0; 104];
        tree_key[..72].copy_from_slice(&source_key);
        tree_key[72..].copy_from_slice(&variant);
        let mut header = b"TSQENT01".to_vec();
        header.extend_from_slice(&source_key[32..]);
        header.extend_from_slice(&identity);
        header.extend_from_slice(&variant);
        Self {
            path,
            source_key,
            tree_key,
            header,
        }
    }

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
        for invalid in ["", ".", "../a", "a/../b", "/a", ".tree-squatter/a"] {
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
            header: vec![7; 184],
        };
        let encoded = request.encode(b"slab");
        assert_eq!(&encoded[184..192], &4u64.to_le_bytes());
        assert_eq!(request.decode(&encoded), Some(b"slab".as_slice()));
        for end in 0..encoded.len() {
            assert!(request.decode(&encoded[..end]).is_none());
        }
        let mut bad = encoded.clone();
        bad[184..192].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(request.decode(&bad).is_none());
        let mut bad = encoded.clone();
        bad.push(0);
        assert!(request.decode(&bad).is_none());
        let mut bad = encoded;
        bad[0] ^= 1;
        assert!(request.decode(&bad).is_none());
    }
}
