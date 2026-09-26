//! Same-build process handoff. This framing is not a durable cache format.
use crate::{
    CacheError, IdentifiedGrammar, LoadedFile, LoadedTree, PendingWrite, Persistence,
    identity::Request,
};
use std::io::{self, Read, Write};
use std::sync::Arc;

// Prototype formats stay at version 0; no persisted data needs backward compatibility.
const TRANSFER_SIGNATURE: &[u8; 8] = b"TSQXFR00";
const HEADER_LEN: usize = 152;
const PREFIX_LEN: usize = 8 + 3 * 8 + HEADER_LEN;

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid publication transfer")
}

impl PendingWrite {
    /// Bytes needed to transfer this capture, including transient tree capacity.
    pub fn transfer_len(&self) -> Option<usize> {
        PREFIX_LEN
            .checked_add(self.request.path.len())?
            .checked_add(self.file.source().len())?
            .checked_add(self.file.tree().as_bytes().len())
    }

    /// Copy the original capture to an IPC sink without compacting the tree,
    /// opening a transaction, or synchronizing storage. The sink must not retain
    /// only a borrowed buffer: publication may outlive this process.
    pub fn write_transfer(&self, mut output: impl Write) -> io::Result<()> {
        output.write_all(TRANSFER_SIGNATURE)?;
        for length in [
            self.request.path.len(),
            self.file.source().len(),
            self.file.tree().as_bytes().len(),
        ] {
            output.write_all(&(length as u64).to_le_bytes())?;
        }
        output.write_all(&self.request.header)?;
        output.write_all(&self.request.path)?;
        output.write_all(self.file.source())?;
        output.write_all(self.file.tree().as_bytes())
    }
}

impl Persistence {
    /// Receive bounded publication work from a trusted same-build producer.
    /// Checks path, captured-byte identity, grammar/runtime/representation, and
    /// structural tree safety before allowing publication. Does not reparse or
    /// read the current source contents; later edits remain separate generations.
    /// Reads exactly one frame without waiting for EOF, leaving subsequent bytes
    /// for the next call. `max_bytes` bounds the total size of this frame.
    pub fn read_transfer(
        &self,
        mut input: impl Read,
        grammar: &IdentifiedGrammar,
        max_bytes: usize,
    ) -> Result<PendingWrite, CacheError> {
        if max_bytes < PREFIX_LEN {
            return Err(invalid().into());
        }
        let mut prefix = [0; PREFIX_LEN];
        input.read_exact(&mut prefix)?;
        if &prefix[..8] != TRANSFER_SIGNATURE {
            return Err(invalid().into());
        }
        let length = |offset| -> io::Result<usize> {
            usize::try_from(u64::from_le_bytes(
                prefix[offset..offset + 8].try_into().unwrap(),
            ))
            .map_err(|_| invalid())
        };
        let path_len = length(8)?;
        let source_len = length(16)?;
        let tree_len = length(24)?;
        let total = PREFIX_LEN
            .checked_add(path_len)
            .and_then(|n| n.checked_add(source_len))
            .and_then(|n| n.checked_add(tree_len))
            .filter(|n| *n <= max_bytes && source_len <= u32::MAX as usize)
            .ok_or_else(invalid)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(total - PREFIX_LEN)
            .map_err(io::Error::other)?;
        bytes.resize(total - PREFIX_LEN, 0);
        input.read_exact(&mut bytes)?;
        let path = &bytes[..path_len];
        let relative = crate::maintenance::decode_path(path).ok_or_else(invalid)?;
        if !self
            .root
            .join(relative)
            .canonicalize()?
            .starts_with(&self.root)
        {
            return Err(invalid().into());
        }
        let source: Arc<[u8]> = bytes[path_len..path_len + source_len].into();
        let mut request = Request::new(
            path.to_vec(),
            &source,
            grammar,
            self.options.symbol_presence,
            self.options.points,
        );
        if request.header.as_slice() != &prefix[32..] {
            return Err(invalid().into());
        }
        let mut tree = tree_sitter_squatter::Tree::from_bytes_safety_checked(
            &grammar.prepared,
            &bytes[path_len + source_len..],
        )
        .map_err(io::Error::other)?;
        if tree
            .root_node()
            .preorder()
            .nodes()
            .any(|node| node.end_byte() > source.len())
        {
            return Err(invalid().into());
        }
        if request.presence {
            let cache = tree_sitter_squatter::PresenceCache::build(&tree, None)
                .map_err(io::Error::other)?;
            tree.set_presence_cache(cache).map_err(io::Error::other)?;
        }
        if request.points {
            let index = tree_sitter_squatter::LineIndex::new(&source).map_err(io::Error::other)?;
            let points = tree_sitter_squatter::PointData::build(&tree, &index, None)
                .map_err(io::Error::other)?;
            tree.set_point_data(points).map_err(io::Error::other)?;
        }
        let store = self
            .store
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cache unavailable"))?;
        request.current_guard = store.current_guard(&request);
        let request = Arc::new(request);
        Ok(PendingWrite {
            store: Some(store.clone()),
            grammar: grammar.clone(),
            file: LoadedFile {
                source,
                tree: LoadedTree::Owned(Arc::new(tree)),
                hit: false,
                cleanup: Some((store, request.clone())),
            },
            request,
        })
    }
}
