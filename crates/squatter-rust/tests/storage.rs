use tree_squatter_rust::{Grammar, PackContext, PackOptions, Tree};

#[test]
fn direct_parser_matches_reference_and_recovers_after_failure() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) };
    let grammar = Grammar::new(&language).unwrap();
    let reference_grammar = tree_squatter::Grammar::new(&language).unwrap();
    let mut parser = tree_squatter_rust::Parser::new(&grammar).unwrap();
    let mut reference = tree_squatter::Parser::new(&reference_grammar).unwrap();
    let wide = "int value = 1;\n".repeat(1000);

    for source in ["", "int x = ;", &wide, "int f() { return 1; }", "int y;"] {
        for points in [false, true] {
            let actual = parser.parse_with_options(
                source,
                PackOptions {
                    points,
                    ..Default::default()
                },
            );
            let expected = reference.parse_with_options(
                source,
                tree_squatter::PackOptions {
                    points,
                    ..Default::default()
                },
            );
            match (actual, expected) {
                (Ok(actual), Ok(expected)) => assert_eq!(actual.as_bytes(), expected.as_bytes()),
                (Err(actual), Err(expected)) => {
                    assert_eq!(actual.code as i32, expected.code as i32);
                    assert_eq!(actual.byte, expected.byte);
                    assert_eq!(actual.point, expected.point);
                    assert_eq!(actual.message, expected.message);
                }
                _ => panic!("different parse result for {source}"),
            }
        }
        parser.trim();
        reference.trim();
    }

    let tree = parser.parse("int values[] = {1, 2, 3};").unwrap();
    drop(parser);
    drop(grammar);
    assert_eq!(
        tree.as_bytes(),
        reference
            .parse("int values[] = {1, 2, 3};")
            .unwrap()
            .as_bytes()
    );
}

#[test]
fn packing_and_loading_match_reference() {
    let json =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let c = unsafe { tree_sitter::Language::from_raw(tree_sitter_c::LANGUAGE.into_raw()().cast()) };
    let c_sharp = unsafe {
        tree_sitter::Language::from_raw(tree_sitter_c_sharp::LANGUAGE.into_raw()().cast())
    };
    let wide = format!("[{}0]", "{\"a\": 1, \"b\": true},\n".repeat(1000));
    let deep = format!("{}0{}", "[".repeat(300), "]".repeat(300));
    let sibling_depths = (29..=33)
        .map(|depth| format!("{}0{}", "[".repeat(depth), "]".repeat(depth)))
        .collect::<Vec<_>>()
        .join(",");
    let inline_boundary = format!("[{sibling_depths}]");
    let long = format!("[\"{}\",\n\"{}\"]", "a".repeat(70000), "b".repeat(400));

    for (language, sources) in [
        (
            json,
            vec![
                "0",
                "{}",
                "{\"a\": [1, true, null]}",
                "[1,",
                &wide,
                &deep,
                &inline_boundary,
                &long,
            ],
        ),
        (
            c,
            vec![
                "",
                "int f(int x) { /* extra */ return x + 1; }",
                "int x = ;",
                "int f() { return 1 }",
            ],
        ),
        (
            c_sharp,
            vec![
                "class C { int F(int x) => x + 1; }",
                "class C { int x = ; }",
            ],
        ),
    ] {
        let grammar = Grammar::new(&language).unwrap();
        let reference_grammar = tree_squatter::Grammar::new(&language).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let mut context = PackContext::new().unwrap();

        for source in sources {
            let tree = parser.parse(source, None).unwrap();
            for points in [false, true] {
                for symbol_presence in [false, true] {
                    for repack in [false, true] {
                        for initial_group_capacity in [0, 1] {
                            let options = PackOptions {
                                initial_group_capacity,
                                repack,
                                symbol_presence,
                                points,
                            };
                            let expected = tree_squatter::Tree::pack_with_options(
                                &reference_grammar,
                                &tree,
                                tree_squatter::PackOptions {
                                    initial_group_capacity,
                                    repack,
                                    symbol_presence,
                                    points,
                                },
                            )
                            .unwrap();
                            let actual =
                                context.pack_with_options(&grammar, &tree, options).unwrap();
                            let description = format!("{} bytes, {options:?}", source.len());
                            assert_eq!(
                                actual.as_bytes().len(),
                                expected.as_bytes().len(),
                                "{description}"
                            );
                            assert!(
                                actual.as_bytes() == expected.as_bytes(),
                                "different slab: {description}, first difference {:?}",
                                actual
                                    .as_bytes()
                                    .iter()
                                    .zip(expected.as_bytes())
                                    .position(|(a, b)| a != b)
                            );

                            let loaded = Tree::from_bytes(&grammar, expected.as_bytes()).unwrap();
                            let borrowed =
                                Tree::from_bytes_borrowed(&grammar, expected.as_bytes()).unwrap();
                            assert_eq!(loaded.as_bytes(), borrowed.as_bytes());
                            tree_squatter::Tree::from_bytes(&reference_grammar, actual.as_bytes())
                                .unwrap();

                            let compact = actual.repack().unwrap();
                            assert_eq!(compact.as_bytes(), expected.repack().unwrap().as_bytes());
                            for group in 0..actual.group_count() {
                                for symbol in 0..language.node_kind_count() as u16 {
                                    assert_eq!(
                                        actual.group_has_symbol(
                                            group,
                                            tree_squatter_rust::KindId::new(symbol)
                                        ),
                                        expected.group_has_symbol(group, symbol)
                                    );
                                }
                            }
                        }
                    }
                }
            }
            context.trim();
        }
    }
}

#[test]
fn node_validation_matches_reference_for_corrupted_slabs() {
    let language =
        unsafe { tree_sitter::Language::from_raw(tree_sitter_json::LANGUAGE.into_raw()().cast()) };
    let grammar = Grammar::new(&language).unwrap();
    let reference = tree_squatter::Grammar::new(&language).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let deep = format!("{}0{}", "[".repeat(70), "]".repeat(70));
    let wide = format!("[{}0]", "[1],".repeat(80));
    let siblings = format!("[{deep},[1],{deep},2]");
    for source in ["[0,[1],2,[],3]", &deep, &wide, &siblings] {
        let native = parser.parse(source, None).unwrap();
        for points in [false, true] {
            let tree = Tree::pack_with_options(
                &grammar,
                &native,
                PackOptions {
                    points,
                    symbol_presence: false,
                    repack: true,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut bytes = tree.as_bytes().to_vec();
            // Keep the layout header fixed while perturbing node columns and group bases.
            for offset in 16..bytes.len() {
                for mask in [1, 128] {
                    bytes[offset] ^= mask;
                    let expected =
                        tree_squatter::Tree::from_bytes_safety_checked(&reference, &bytes).is_ok();
                    assert_eq!(
                        Tree::from_bytes_safety_checked(&grammar, &bytes).is_ok(),
                        expected,
                        "offset {offset}, mask {mask}, points {points}"
                    );
                    bytes[offset] ^= mask;
                }
            }
        }
    }
}
