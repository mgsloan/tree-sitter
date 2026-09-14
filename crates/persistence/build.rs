use std::{env, fs, path::Path};

// Conservative development-build identity. Release compatibility epochs can
// replace this later; grammar identity is independently supplied by the caller.
fn hash_tree(path: &Path, root: &Path, hash: &mut blake3::Hasher) {
    println!("cargo:rerun-if-changed={}", path.display());
    if path.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for entry in entries {
            hash_tree(&entry, root, hash);
        }
    } else if matches!(
        path.extension().and_then(|s| s.to_str()),
        Some("c" | "h" | "rs")
    ) {
        let name = path.strip_prefix(root).unwrap().to_str().unwrap();
        let bytes = fs::read(path).unwrap();
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(&(bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
}

fn main() {
    let root = Path::new("../..");
    let mut hash = blake3::Hasher::new_derive_key("tree-squatter persistence development build v1");
    for path in [
        "lib/src",
        "lib/include",
        "lib/squat",
        "lib/binding_rust/build.rs",
        "crates/squatter/build.rs",
    ] {
        hash_tree(&root.join(path), root, &mut hash);
    }
    for name in ["TARGET", "CFLAGS", "TARGET_CFLAGS", "CC"] {
        println!("cargo:rerun-if-env-changed={name}");
        let value = env::var(name).unwrap_or_default();
        hash.update(&(value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    println!(
        "cargo:rustc-env=TSQ_RUNTIME_FINGERPRINT={}",
        hash.finalize().to_hex()
    );
}
