use tree_squatter::{FieldId, FieldSet, KindId, Language, PackOptions, Query, Tree};

#[test]
fn invalid_kinds_are_rejected() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse("?", None).unwrap();
    let error = KindId::ERROR;
    let symbol_count = language.node_kind_count() as u16;
    assert_eq!(grammar.kind_id_for_name("ERROR", true), Some(error));
    for symbol_presence in [false, true] {
        let tree = Tree::pack_with_options(
            &grammar,
            &native,
            PackOptions {
                symbol_presence,
                ..Default::default()
            },
        )
        .unwrap();
        let root = tree.root_node();
        assert!(root.all().filter_kind_ids([error]).count() > 0);
        assert!((0..tree.group_count()).any(|group| tree.group_has_symbol(group, error)));
        for raw in [symbol_count, symbol_count + 1, 32768, u16::MAX - 2] {
            let invalid = KindId::new(raw);
            assert_eq!(root.all().filter_kind_ids([invalid]).count(), 0);
            for group in 0..tree.group_count() {
                assert!(!tree.group_has_symbol(group, invalid));
            }
        }
    }
}

#[test]
fn typed_fields_and_slot_lookup() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse(r#"{"key": 1}"#, None).unwrap();
    let tree = Tree::pack(&grammar, &native).unwrap();
    let root = tree.root_node();
    let key = grammar.field_id_for_name("key").unwrap();
    let pair = root.named_child(0).unwrap().named_child(0).unwrap();
    let child = pair.child_by_field_id(key).unwrap();
    assert_eq!(child.field_id(), Some(key));
    assert_eq!(
        child.kind_id(),
        grammar.kind_id_for_name("string", true).unwrap()
    );
    assert_eq!(tree.node_at_slot(child.slot()), Some(child));
    assert_eq!(root.field_id(), None);
    assert_eq!(FieldId::new(0), None);
    assert_eq!(grammar.field_id_for_name("unknown"), None);
    assert_eq!(grammar.kind_id_for_name("unknown", true), None);
    assert_eq!(grammar.kind_id_for_name("string", false), None);

    assert_eq!(
        root.all()
            .filter_field_ids([key])
            .nodes()
            .collect::<Vec<_>>(),
        [child]
    );
    let fields = FieldSet::new([None, Some(key)]);
    let selected = root
        .all()
        .filter_field_ids(&fields)
        .nodes()
        .collect::<Vec<_>>();
    let expected = std::iter::successors(Some(root), |node| node.next_preorder())
        .filter(|node| node.field_id().is_none() || node.field_id() == Some(key))
        .collect::<Vec<_>>();
    assert_eq!(selected, expected);
}

#[test]
fn grammar_kind_lookup_ignores_aliases() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let native = parser.parse("typedef int T; T value;", None).unwrap();
    let tree = Tree::pack(&grammar, &native).unwrap();
    let kind = grammar.kind_id_for_name("type_identifier", true).unwrap();
    let original = grammar
        .grammar_kind_id_for_name("identifier", true)
        .unwrap();
    let node = tree
        .root_node()
        .all()
        .filter_kind_ids([kind])
        .nodes()
        .next()
        .unwrap();
    assert_eq!(node.kind_id(), kind);
    assert_eq!(node.grammar_id(), original);
    assert_eq!(
        grammar.grammar_kind_id_for_name("type_identifier", true),
        None
    );
    assert_eq!(grammar.grammar_kind_id_for_name("unknown", true), None);
    assert_eq!(grammar.grammar_kind_id_for_name("identifier", false), None);
    assert_eq!(
        grammar
            .grammar_kind_id_for_name("ERROR", true)
            .unwrap()
            .get(),
        u16::MAX
    );
}

#[test]
fn compiler_metadata_and_mutation_match_tree_sitter() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Language::new(&language).unwrap();
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
        let reference = tree_sitter::Query::new(&language, source);
        let candidate = Query::new(&grammar, source);
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
                if let Some(name) = reference
                    .capture_names()
                    .first()
                    .map(|name| (*name).to_owned())
                {
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
                assert!(!candidate.message.is_empty(), "{source}");
            }
            _ => panic!("different compilation result for {source}"),
        }
    }
}

#[test]
fn language_cache_round_trips_and_outlives_tree_sitter_language() {
    let language = unsafe {
        tree_sitter::Language::from_raw(tree_sitter_c_sharp::LANGUAGE.into_raw()().cast())
    };
    let candidate = Language::new(&language).unwrap();
    let bytes = candidate.cache().unwrap();
    let restored = Language::from_cache(&language, &bytes).unwrap();
    let clone = restored.clone();
    drop(restored);
    drop(candidate);
    drop(language);
    assert_eq!(clone.cache().unwrap(), bytes);
}
