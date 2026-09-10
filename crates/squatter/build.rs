fn main() {
    let root = std::path::Path::new("../../lib/squat");
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include("../../lib/include")
        .include("../../lib/src");
    for file in [
        "slab.c",
        "pack.c",
        "node.c",
        "cursor.c",
        "iterator.c",
        "unpack.c",
        "index.c",
        "scan.c",
        "query.c",
    ] {
        build.file(root.join(file));
        println!("cargo:rerun-if-changed={}", root.join(file).display());
    }
    for file in [
        "attributes.h",
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
    let points = if std::env::var_os("CARGO_FEATURE_POINTS").is_some() {
        "1"
    } else {
        "0"
    };
    build.define("SQ_INCLUDE_POINTS", points);
    // Catch contradictory CFLAGS before Rust and C can disagree on snapshot ABI.
    let guard =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("point_config.c");
    let guard_source = format!(
        r#"#include <tree_sitter/squat.h>
_Static_assert(SQ_INCLUDE_POINTS == {points},
               "use the Cargo points feature to configure row/column support");
"#
    );
    std::fs::write(&guard, guard_source).unwrap();
    build.include(root.join("include")).file(guard);
    build.warnings(true).compile("tree-sitter-squat");
}
