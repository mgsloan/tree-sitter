use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tree_sitter_squatter::{PackOptions, Tree};

struct TrackedSlab {
    storage: Box<[u64]>,
    offset: usize,
    length: usize,
    drops: Arc<AtomicUsize>,
}
impl Drop for TrackedSlab {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}
// Heap allocation is stable across owner moves and never mutated after creation.
unsafe impl tree_sitter_squatter::StableSlab for TrackedSlab {
    fn bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self.storage.as_ptr().cast::<u8>().add(self.offset),
                self.length,
            )
        }
    }
}
fn tracked(bytes: &[u8], misaligned: bool, drops: Arc<AtomicUsize>) -> TrackedSlab {
    let alignment = 8;
    let mut storage = vec![0u64; (bytes.len() + alignment + 8).div_ceil(8)].into_boxed_slice();
    let offset = storage.as_ptr().cast::<u8>().align_offset(alignment) + usize::from(misaligned);
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            storage.as_mut_ptr().cast::<u8>().add(offset),
            bytes.len(),
        );
    }
    TrackedSlab {
        storage,
        offset,
        length: bytes.len(),
        drops,
    }
}

#[test]
fn owned_slab_retains_storage_and_releases_it_on_all_outcomes() {
    let tree_sitter_language = tree_sitter_language();
    let language = tree_sitter_squatter::Language::new(&tree_sitter_language).unwrap();
    let tree = pack(&tree_sitter_language, "[42]", false);
    let drops = Arc::new(AtomicUsize::new(0));
    let owner = tracked(tree.as_bytes(), false, drops.clone());
    let address = tree_sitter_squatter::StableSlab::bytes(&owner).as_ptr();
    let backed = Tree::from_owned_slab(&language, owner).unwrap();
    assert_eq!(backed.as_bytes().as_ptr(), address);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    let detached = backed.detach().unwrap();
    assert_ne!(detached.as_bytes().as_ptr(), address);
    drop(backed);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(detached.root_node().kind(), "document");
    assert!(
        Tree::from_owned_slab(&language, tracked(tree.as_bytes(), true, drops.clone())).is_err()
    );
    assert_eq!(drops.load(Ordering::Relaxed), 2);
    assert!(Tree::from_owned_slab(&language, tracked(b"invalid", false, drops.clone())).is_err());
    assert_eq!(drops.load(Ordering::Relaxed), 3);
}

fn tree_sitter_language() -> tree_sitter::Language {
    unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) }
}

fn pack(tree_sitter_language: &tree_sitter::Language, source: &str, presence: bool) -> Tree {
    let language = tree_sitter_squatter::Language::new(tree_sitter_language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(tree_sitter_language).unwrap();
    Tree::pack_with_options(
        &language,
        &parser.parse(source, None).unwrap(),
        PackOptions {
            repack: true,
            symbol_presence: presence,
            ..PackOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn presence_sidecar_is_separate_and_debug_loading_checks_membership() {
    let tree_sitter_language = tree_sitter_language();
    let language = tree_sitter_squatter::Language::new(&tree_sitter_language).unwrap();
    let source = format!("[{}0]", "1,".repeat(4096));
    let original = pack(&tree_sitter_language, &source, true);
    let without = pack(&tree_sitter_language, &source, false);
    assert_eq!(original.as_bytes(), without.as_bytes());
    let cache = original.presence_cache().unwrap();
    let mut corrupted = cache.as_bytes().to_vec();
    corrupted[16..].fill(0);
    let loaded = Tree::from_bytes_safety_checked(&language, original.as_bytes()).unwrap();
    assert!(!loaded.has_points());
    assert!(loaded.presence_cache().is_none());
    assert_eq!(
        tree_sitter_squatter::PresenceCache::copy_from_bytes(&loaded, &corrupted).is_err(),
        cfg!(debug_assertions),
    );
    assert!(
        tree_sitter_squatter::PresenceCache::copy_from_bytes(&loaded, cache.as_bytes()).is_ok()
    );
}

#[test]
fn safety_loader_rejects_truncated_sections_and_invalid_headers() {
    let tree_sitter_language = tree_sitter_language();
    let language = tree_sitter_squatter::Language::new(&tree_sitter_language).unwrap();
    let original = pack(&tree_sitter_language, "{\"key\": [true, 42]}", true);
    for length in 0..original.as_bytes().len() {
        assert!(
            Tree::from_bytes_safety_checked(&language, &original.as_bytes()[..length]).is_err()
        );
    }
    for (offset, value) in [(0, 0), (4, 0), (8, u32::MAX), (12, 257)] {
        let mut bytes = original.as_bytes().to_vec();
        bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
        assert!(Tree::from_bytes_safety_checked(&language, &bytes).is_err());
    }
}

#[test]
fn copied_loaders_handle_trees_deeper_than_inline_validation_storage() {
    let tree_sitter_language = tree_sitter_language();
    let language = tree_sitter_squatter::Language::new(&tree_sitter_language).unwrap();
    let source = format!("{}0{}", "[".repeat(128), "]".repeat(128));
    let original = pack(&tree_sitter_language, &source, false);
    let nodes = original.root_node().preorder().count();
    for loaded in [
        Tree::from_bytes(&language, original.as_bytes()).unwrap(),
        Tree::from_bytes_safety_checked(&language, original.as_bytes()).unwrap(),
    ] {
        assert_eq!(loaded.root_node().preorder().count(), nodes);
        assert_eq!(loaded.as_bytes(), original.as_bytes());
    }
}

#[test]
fn mutated_slabs_are_rejected_or_support_bounded_traversal() {
    let tree_sitter_language = tree_sitter_language();
    let language = tree_sitter_squatter::Language::new(&tree_sitter_language).unwrap();
    let original = pack(&tree_sitter_language, "{\"key\": [true, 42]}", false);
    let mut state = 42u32;
    for trial in 0..512 {
        let mut bytes = original.as_bytes().to_vec();
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let index = state as usize % bytes.len();
        bytes[index] ^= 1 << (trial % 8);
        let Ok(tree) = Tree::from_bytes_safety_checked(&language, &bytes) else {
            continue;
        };
        for (count, node) in tree.root_node().preorder().nodes().enumerate() {
            assert!(count < tree.slot_count() as usize);
            let _ = (node.kind(), node.grammar_name(), node.field_name());
            let _ = (node.parent(), node.next_sibling(), node.prev_sibling());
            let _ = (
                node.child_count(),
                node.named_child_count(),
                node.descendant_count(),
            );
            assert!(node.start_byte() <= node.end_byte());
        }
    }
}

#[test]
fn shared_grammars_support_concurrent_packing_and_outlive_handles() {
    let json = tree_sitter_squatter::Language::new(&tree_sitter_language()).unwrap();
    let c_sharp_language = unsafe {
        tree_sitter::Language::from_raw(tree_sitter_c_sharp::LANGUAGE.into_raw()().cast())
    };
    let c_sharp = tree_sitter_squatter::Language::new(&c_sharp_language).unwrap();
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let json = json.clone();
            let c_sharp = c_sharp.clone();
            std::thread::spawn(move || {
                let mut parser = tree_sitter::Parser::new();
                let mut context = tree_sitter_squatter::PackContext::new().unwrap();
                let mut retained = Vec::new();
                for _ in 0..4 {
                    for (language, source) in
                        [(&json, "{\"a\": [1, 2]}"), (&c_sharp, "class C { int x; }")]
                    {
                        parser
                            .set_language(&language.tree_sitter_language())
                            .unwrap();
                        let native = parser.parse(source, None).unwrap();
                        let tree = context.pack(language, &native).unwrap();
                        let reference = Tree::pack(language, &native).unwrap();
                        assert_eq!(tree.as_bytes(), reference.as_bytes());
                        retained.push(tree);
                    }
                }
                context.drop_scratch();
                retained
            })
        })
        .collect();
    drop(json);
    drop(c_sharp);
    for worker in workers {
        for tree in worker.join().unwrap() {
            assert!(tree.root_node().descendant_count() > 1);
            let compact = tree.repack().unwrap();
            assert_eq!(
                compact.root_node().attributes(),
                tree.root_node().attributes()
            );
            assert_eq!(
                compact.root_node().preorder().count(),
                tree.root_node().preorder().count()
            );
            assert!(!tree.root_node().has_error());
        }
    }
}
