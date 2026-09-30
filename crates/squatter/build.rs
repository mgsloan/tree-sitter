use std::{collections::BTreeSet, env, fs, path::PathBuf};

fn main() {
    let header = fs::read_to_string("native/query.h").unwrap();
    let mut flags = String::new();
    for line in header
        .lines()
        .filter(|line| line.starts_with("#define SQ_STEP_"))
    {
        let mut words = line.split_whitespace();
        words.next();
        let name = words.next().unwrap().strip_prefix("SQ_STEP_").unwrap();
        let bit: u32 = words.last().unwrap().trim_end_matches(')').parse().unwrap();
        flags.push_str(&format!("pub const {name}: u16 = 1 << {bit};\n"));
    }
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("query_flags.rs"),
        flags,
    )
    .unwrap();
    let include = PathBuf::from(env::var_os("DEP_TREE_SITTER_INCLUDE").unwrap());
    let source = include.parent().unwrap().join("src");
    bindgen::Builder::default()
        .header(source.join("subtree.h").to_str().unwrap())
        .clang_arg(format!("-I{}", include.display()))
        .clang_arg(format!("--target={}", env::var("TARGET").unwrap()))
        .allowlist_type("Subtree")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate_comments(false)
        .layout_tests(true)
        .generate()
        .expect("generate Tree-sitter subtree layout")
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("subtree.rs"))
        .unwrap();
    println!("cargo:rerun-if-changed={}", source.display());
    let feller = PathBuf::from("native/tree_feller");
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include(include)
        .include(source)
        .include("native")
        .include(feller.join("include"))
        .include(feller.join("src"));
    if !cfg!(debug_assertions) {
        build.define("NDEBUG", None);
    }

    let mut names = BTreeSet::new();
    for file in [
        "include/tree_feller.h",
        "src/tf_internal.h",
        "src/tf_language.c",
        "src/tf_lexer.c",
        "src/tf_parser.c",
    ] {
        let path = feller.join(file);
        if let Ok(text) = fs::read_to_string(&path) {
            for name in
                text.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            {
                if name.starts_with("tf_") {
                    names.insert(name.to_owned());
                }
            }
        }
    }
    for name in names {
        build.define(&name, format!("sq_native_{name}").as_str());
    }
    for file in [
        "grammar.c",
        "supertypes.c",
        "query.c",
        "parser.c",
        "tests.c",
    ] {
        build.file(PathBuf::from("native").join(file));
    }
    for file in ["tf_language.c", "tf_lexer.c", "tf_parser.c"] {
        build.file(feller.join("src").join(file));
    }

    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-changed={}", feller.display());
    build.compile("squatter-native");
}
