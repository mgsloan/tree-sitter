# Inherited field lookup: comparison with ../main

The packed engine in `../main` does **not** handle this particular case. Fresh
builds of its packed engine and its upstream Tree-sitter engine disagree on:

```typescript
type Example = typeof object.property;
```

Results when calling `child_by_field_id` on the `type_query` node:

| Field | Upstream Tree-sitter | ../main packed engine | Squat with exceptions |
|---|---|---|---|
| `object` | `identifier`, bytes 22..28 | null | `identifier`, bytes 22..28 |
| `property` | `property_identifier`, bytes 29..37 | null | `property_identifier`, bytes 29..37 |

The direct visible children of `type_query` are `typeof` and an aliased
`member_expression`. Both have cursor field ID zero. The latter contains the
`object` and `property` children. All other printed nodes, child fields, and
successful field lookups agree between the two `../main` builds in this probe.

## How ../main works

[`ts_frame_parser_field_for_position`](../../../../main/lib/src/squatter/frame_parser.c)
ignores inherited field-map entries and keeps direct assignments.
[`frame_walk.c`](../../../../main/lib/src/squatter/frame_walk.c) propagates an
ambient field through hidden splices; a child's direct assignment takes
precedence. A visible or aliased composite gets its own record and starts a new
field context for its children. The mainline-to-packed converter in
[`api_packed.c`](../../../../main/lib/src/squatter/api_packed.c) follows the same
propagation rule; the executable probe here exercises the packed parser.

At lookup time, `squatter_node_child_by_field_id` calls
[`ts_block_cursor_goto_child_with_field`](../../../../main/lib/src/squatter/block_cursor.c),
which scans direct visible children and compares their stored field IDs. There
is no inherited-field recursion or exception lookup. The comment in
`frame_parser.c` claiming identical reconstruction does not hold for this alias
case. Optional reparse production metadata is not consulted by this reader.

Upstream [`node.c`](../../../../main/lib/src/node.c) instead checks the grammar
production's **inherited** field entry before testing the child's visible/alias
status. It descends through that child even though aliasing makes it visible,
so the result can be a grandchild in the public tree.

Simply searching all descendants would also be wrong. The control input
`object.property;` has an `expression_statement` containing a `member_expression`,
but looking up either field on the statement correctly returns null. Grammar
inheritance determines which visible boundaries lookup may cross.

## Potential upstream inconsistency

A subsequent contract review checked the same grammar pin's generated
`typescript/src/node-types.json`: `type_query` declares `"fields": {}`. Its
runtime nevertheless exposes `object` and `property` through the visible alias.
That is a potential upstream field-map/alias/API inconsistency. The compatibility
comparison above does not establish that upstream's behavior is intended; the
packed engine's null results may better match the visible-tree contract.
See entry 7 in [the upstream bug list](../../../../potential-upstream-bugs.md).

## Assessment of the sparse section

Keep the extension while exact compatibility with current upstream is required.
A cursor field and a field-lookup target express different
relationships here. Some additional information is needed to reproduce both;
the **specific 40-byte header and 12-byte triples are a design choice**, not the
only possible encoding. Preserving enough grammar-production and hidden-child
structure to replay lookup would be an alternative, with broader storage and
navigation consequences for this visible-node layout.

[`pack.c`](../pack.c) computes the grammar lookup and the ordinary visible-child
lookup separately, bottom-up, and records only disagreements. It uses temporary
field-target lists in open traversal frames, rather than a full-tree pointer
map. [`index.c`](../index.c) stores sorted exceptions, checks their field IDs,
ordering, and descendant targets on load, and binary-searches them at lookup.
A null target suppresses an ordinary child match when grammar lookup has none.

For this exact input the section contains two records, **24 bytes**:

```text
(parent slot 9, field object,   target slot 12)
(parent slot 9, field property, target slot 14)
```

The default layout additionally pays **8 header bytes per tree**, even when
there are no exceptions. The earlier 27-file layout sample had 48 bytes of
exception records in total; that figure excludes the per-tree header increase.
The control input needs no exceptions. This is independent of seek differences.
The format is unchanged by this review.

## Reproduction

Verified against `../main` commit
`c1ce0f4f166dad57cd18aa684ded2f701ec02299`, with no changes under its `lib/src` or
`lib/include`. The TypeScript grammar pin is
`e2c53597d6a5d9cf7bbe8dccde576fe1e46c5899`; the tested grammar library SHA-256 is
`7ede8b627e1c60fdd8acecb27cdab5e09513e990752a02bf10e51084c7143406`.

From the pareto checkout, compile the public-API [probe](field-lookup.c) separately
against each engine. These commands use the grammar built by the earlier corpus
checks; substitute another matching TypeScript grammar library if necessary.

```sh
mkdir -p build/field-lookup-review
cc -O1 -g -std=c11 -D_DEFAULT_SOURCE -I../main/lib/src -I../main/lib/include \
  -c ../main/lib/src/squatter/lib.c -o build/field-lookup-review/main-squatter.o
cc -O1 -g -std=c11 -D_DEFAULT_SOURCE -I../main/lib/src -I../main/lib/include \
  -c ../main/lib/src/lib.c -o build/field-lookup-review/main-upstream.o
cc -O2 -I../main/lib/include lib/squat/experiments/field-lookup.c \
  build/field-lookup-review/main-squatter.o -ldl -o build/field-lookup-review/main-squatter
cc -O2 -I../main/lib/include lib/squat/experiments/field-lookup.c \
  build/field-lookup-review/main-upstream.o -ldl -o build/field-lookup-review/main-upstream
make -C lib/squat all
cc -O2 -DWITH_SQUAT -Ilib/include -Ilib/squat/include \
  lib/squat/experiments/field-lookup.c build/squat/libtree-sitter-squat.a \
  build/squat/runtime.o -ldl -o build/field-lookup-review/mainline-and-squat
build/field-lookup-review/main-squatter build/squat-parent-index/typescript.so
build/field-lookup-review/main-upstream build/squat-parent-index/typescript.so
build/field-lookup-review/mainline-and-squat build/squat-parent-index/typescript.so
build/field-lookup-review/mainline-and-squat build/squat-parent-index/typescript.so 'object.property;'
```
