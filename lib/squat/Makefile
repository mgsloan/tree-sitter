ROOT := ../..
CC ?= cc
CFLAGS ?= -O2 -g
CPPFLAGS += -I$(ROOT)/lib/include -I$(ROOT)/lib/src -Iinclude
WARNINGS := -std=c11 -Wall -Wextra -Werror
SOURCES := slab.c pack.c node.c cursor.c iterator.c unpack.c index.c scan.c query.c
BUILD ?= $(ROOT)/build/squat

.PHONY: all clean test
all: $(BUILD)/libtree-sitter-squat.a $(BUILD)/compare

$(BUILD):
	mkdir -p $@

$(BUILD)/%.o: %.c internal.h attributes.h include/tree_sitter/squat.h | $(BUILD)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) -c $< -o $@

$(BUILD)/query.o: query_plan.c query_internal.h include/tree_sitter/squat_query.h

$(BUILD)/libtree-sitter-squat.a: $(SOURCES:%.c=$(BUILD)/%.o)
	$(AR) rcs $@ $^

$(BUILD)/runtime.o: $(ROOT)/lib/src/lib.c | $(BUILD)
	$(CC) $(CPPFLAGS) $(CFLAGS) -std=c11 -D_DEFAULT_SOURCE -c $< -o $@

$(BUILD)/compare: tests/compare.c tests/field_lookup.h $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $(filter %.c %.a %.o,$^) -ldl -o $@

test: $(BUILD)/compare
	$(BUILD)/compare $(GRAMMAR_LIBRARY) $(GRAMMAR_SYMBOL) $(SOURCES_TO_TEST)

clean:
	rm -f $(BUILD)/*.o $(BUILD)/*.a $(BUILD)/compare

$(BUILD)/unit: tests/unit.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -o $@

$(BUILD)/seek-check: tests/seek.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -ldl -o $@

.PHONY: check
check: $(BUILD)/unit
	$(BUILD)/unit

$(BUILD)/scan-bench: experiments/scan.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -o $@

$(BUILD)/layout-bench: experiments/layout.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -ldl -o $@

$(BUILD)/query-check: tests/query.c tests/field_lookup.h $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $(filter %.c %.a %.o,$^) -ldl -o $@

$(BUILD)/unpack-bench: experiments/unpack.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -o $@

# GCC/x86-64 probe; its private columns retain the production encoding.
$(BUILD)/zero-base-bench: experiments/zero-base.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -ldl -o $@

$(BUILD)/end255-bench: experiments/end255.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -ldl -o $@

# Batches of already-parsed trees, so per-tree setup shows against per-node work.
$(BUILD)/setup-bench: experiments/setup.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -ldl -o $@

$(BUILD)/context-check: tests/context.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -ldl \
	  -Wl,--wrap=malloc,--wrap=calloc,--wrap=realloc -o $@

# Repeated sq_tree_from_bytes over pre-serialized slabs: the persistence read path.
$(BUILD)/load-bench: experiments/load.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -ldl -o $@

# GNU ld wrapping counts runtime and Squatter allocations without changing their
# sizes. Prebuilt grammar-internal libc calls are outside this accounting.
$(BUILD)/memory-bench: experiments/memory.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -ldl \
	  -Wl,--wrap=malloc,--wrap=calloc,--wrap=realloc,--wrap=aligned_alloc,--wrap=free -o $@

# Public-API probe for comparing persisted bytes with another build.
$(BUILD)/slab-compatibility: tests/slab-compatibility.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -ldl -o $@
