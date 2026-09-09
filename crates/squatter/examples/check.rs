//! Run inside a grammar container: check LIBRARY SYMBOL.
use std::error::Error;
use tree_sitter_squatter::{
    PackOptions, Tree,
    traits::{NodeLike, TreeLike},
};

fn kinds<T: TreeLike>(tree: &T) -> Vec<u16> {
    fn visit<'tree, N: NodeLike<'tree>>(node: N, output: &mut Vec<u16>) {
        output.push(node.attributes().kind_id);
        let mut index = 0;
        while let Some(child) = node.child(index) {
            visit(child, output);
            index += 1;
        }
    }
    let mut output = Vec::new();
    visit(tree.root(), &mut output);
    output
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.len() != 2 {
        return Err("usage: check LIBRARY SYMBOL".into());
    }
    // The library outlives every language, parser and packed tree in this scope.
    let library = unsafe { libloading::Library::new(&arguments[0])? };
    let get_language: libloading::Symbol<
        unsafe extern "C" fn() -> *const tree_sitter::ffi::TSLanguage,
    > = unsafe { library.get(arguments[1].as_bytes())? };
    let language = unsafe { tree_sitter::Language::from_raw(get_language()) };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language)?;
    let source = b"{\"a\": [1, true, null], \"b\": 2}";
    let mainline = parser.parse(source, None).ok_or("parse failed")?;
    let packed = Tree::pack_with_options(
        &mainline,
        PackOptions {
            initial_group_capacity: 1,
            ..Default::default()
        },
    )?;
    assert_eq!(kinds(&mainline), kinds(&packed));
    assert_eq!(
        packed.root_node().attributes(),
        mainline.root_node().attributes()
    );
    assert_eq!(
        packed.root_node().preorder().count(),
        packed.root_node().descendant_count()
    );
    for node in packed.root_node().preorder() {
        assert_eq!(node.children().count(), node.child_count());
        assert_eq!(node.named_children().count(), node.named_child_count());
        assert_eq!(node.preorder().count(), node.descendant_count());
    }
    drop(mainline);
    let compact = packed.repack()?;
    let decoded = Tree::from_bytes(&language, compact.as_bytes())?;
    assert_eq!(kinds(&packed), kinds(&decoded));
    assert_eq!(compact.group_count(), compact.group_capacity());
    let mut corrupted = compact.as_bytes().to_vec();
    corrupted[0] ^= 0x80;
    assert!(Tree::from_bytes(&language, &corrupted).is_err());
    println!(
        "ok: Rust FFI, ownership, traits, iterators and persistence ({} nodes)",
        packed.root_node().descendant_count()
    );
    Ok(())
}
