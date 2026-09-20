fn main() {
    let root = std::path::Path::new("../../lib/squat");
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include("../../lib/include")
        .include("../../lib/src")
        .include("../../lib/tree_feller/include");
    for file in [
        "symbols.c",
        "supertypes.c",
        "slab.c",
        "pack.c",
        "parser.c",
        "node.c",
        "cursor.c",
        "index.c",
        "scan.c",
        "query.c",
    ] {
        build.file(root.join(file));
        println!("cargo:rerun-if-changed={}", root.join(file).display());
    }
    for file in [
        "attributes.h",
        "reductions.h",
        "query_plan.c",
        "query_internal.h",
        "include/tree_sitter/squat_query.h",
    ] {
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
    for file in ["tf_language.c", "tf_lexer.c", "tf_parser.c"] {
        build.file(std::path::Path::new("../../lib/tree_feller/src").join(file));
    }
    println!("cargo:rerun-if-changed=../../lib/tree_feller");
    build.warnings(true).compile("tree-sitter-squat");
}
