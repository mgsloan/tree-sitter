use std::ffi::{CStr, c_char};

use tree_sitter::{Tree, ffi::TSTree};

pub use crate::fuzz::allocations;
pub mod edits;
pub(super) mod fixtures;
pub(super) mod query_helpers;

unsafe extern "C" {
    fn ts_test_tree_string(tree: *const TSTree) -> *mut c_char;
    fn ts_test_free_string(string: *mut c_char);
}

/// Get the full S-expression, including hidden and anonymous nodes.
pub fn to_sexp_with_hidden_nodes(tree: &Tree) -> String {
    // The borrowed tree stays alive throughout the call. Copy the NUL-terminated
    // result, then release it using the same allocator that created it.
    let bytes = unsafe {
        let string = ts_test_tree_string(tree.root_node().into_raw().tree);
        let bytes = CStr::from_ptr(string).to_bytes().to_owned();
        ts_test_free_string(string);
        bytes
    };
    String::from_utf8(bytes).unwrap()
}
