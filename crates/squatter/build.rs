fn main() {
    let root = std::path::Path::new("../../lib/squat");
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include("../../lib/include")
        .include("../../lib/src");
    for file in ["slab.c", "pack.c", "node.c", "index.c", "scan.c"] {
        build.file(root.join(file));
        println!("cargo:rerun-if-changed={}", root.join(file).display());
    }
    println!(
        "cargo:rerun-if-changed={}",
        root.join("internal.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join("include/tree_sitter/squat.h").display()
    );
    build.warnings(true).compile("tree-sitter-squat");
}
