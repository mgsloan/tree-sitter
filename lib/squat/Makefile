ROOT := ../..
CC ?= cc
CFLAGS ?= -O2 -g
CPPFLAGS += -I$(ROOT)/lib/include -I$(ROOT)/lib/src -Iinclude
WARNINGS := -std=c11 -Wall -Wextra -Werror
SOURCES := symbols.c supertypes.c slab.c pack.c node.c cursor.c iterator.c index.c scan.c query.c
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

$(BUILD)/compare: tests/compare.c tests/field_lookup.h tests/language_clone.h $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $(filter %.c %.a %.o,$^) -ldl -o $@

test: $(BUILD)/compare
	$(BUILD)/compare $(GRAMMAR_LIBRARY) $(GRAMMAR_SYMBOL) $(SOURCES_TO_TEST)

clean:
	rm -f $(BUILD)/*.o $(BUILD)/*.a $(BUILD)/compare

$(BUILD)/unit: tests/unit.c tests/supertype_fixture.h $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $(filter %.c %.a %.o,$^) -o $@

$(BUILD)/seek-check: tests/seek.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -ldl -o $@

.PHONY: check
$(BUILD)/supertype-check: tests/supertypes.c tests/supertype_fixture.h pack.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $(filter-out pack.c,$(filter %.c %.a %.o,$^)) -pthread -o $@

check: $(BUILD)/unit $(BUILD)/supertype-check
	$(BUILD)/unit
	$(BUILD)/supertype-check

$(BUILD)/query-check: tests/query.c tests/field_lookup.h $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $(filter %.c %.a %.o,$^) -ldl -o $@

$(BUILD)/context-check: tests/context.c tests/supertype_fixture.h tests/language_clone.h $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $(filter %.c %.a %.o,$^) -ldl \
	  -Wl,--wrap=malloc,--wrap=calloc,--wrap=realloc -o $@

# Public-API probe for comparing persisted bytes with another build.
$(BUILD)/slab-compatibility: tests/slab-compatibility.c $(BUILD)/libtree-sitter-squat.a $(BUILD)/runtime.o
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) $^ -ldl -o $@
