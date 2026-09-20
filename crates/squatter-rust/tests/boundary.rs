use tree_squatter_rust::{Grammar, Query};

#[test]
fn compiler_metadata_and_mutation_match_reference() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    for source in [
        "(_) @node",
        "(pair key: (string) @key value: (_) @value)",
        "(array [(number) (string)]+ @item)",
        "((string) @text (#match? @text \"a\"))",
        "(array . (number)? @first . (number)* @rest .)",
        "(not_a_node) @capture",
        "(pair invalid_field: (_))",
        "(",
    ] {
        let reference = tree_squatter::Query::new(&language, source);
        let candidate = Query::new(&language, source);
        match (reference, candidate) {
            (Ok(mut reference), Ok(mut candidate)) => {
                assert_eq!(
                    reference.pattern_count(),
                    candidate.pattern_count(),
                    "{source}"
                );
                assert_eq!(
                    reference.capture_names(),
                    candidate.capture_names(),
                    "{source}"
                );
                if let Some(name) = reference.capture_names().first().cloned() {
                    reference.disable_capture(&name);
                    candidate.disable_capture(&name);
                }
                if reference.pattern_count() > 0 {
                    reference.disable_pattern(0);
                    candidate.disable_pattern(0);
                }
            }
            (Err(reference), Err(candidate)) => {
                assert_eq!(reference.offset, candidate.offset, "{source}");
                assert_eq!(reference.message, candidate.message, "{source}");
            }
            _ => panic!("different compilation result for {source}"),
        }
    }
}

#[test]
fn grammar_cache_cross_loads_and_outlives_language() {
    let language = unsafe {
        tree_sitter::Language::from_raw(tree_sitter_c_sharp::LANGUAGE.into_raw()().cast())
    };
    let reference = tree_squatter::Grammar::new(&language).unwrap();
    let candidate = Grammar::new(&language).unwrap();
    let bytes = reference.cache().unwrap();
    assert_eq!(candidate.cache().unwrap(), bytes);
    let restored = Grammar::from_cache(&language, &bytes).unwrap();
    let clone = restored.clone();
    drop(restored);
    drop(candidate);
    drop(language);
    assert_eq!(clone.cache().unwrap(), bytes);
}
