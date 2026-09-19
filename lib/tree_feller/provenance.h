// Vendored from https://github.com/Dekker1/tree-feller
// Commit: d633182bd39ff96effcd1e3123362a2783f45606 (version 0.2.0).
// Local changes:
// - tree_feller.h and tf_parser.c add reusable parser storage.
// - tf_parser.c checks stack allocation sizes and initializes empty diagnostics.
// - tf_lexer.c retries the error-state lexer, preserving mainline token caching.
// - tf_language.c separates driver tables from optional visible-sink metadata.
// Other upstream sources are unmodified. See LICENSE for MIT terms.
// Only tf_language.c, tf_lexer.c, and tf_parser.c are linked by tree-squatter.
