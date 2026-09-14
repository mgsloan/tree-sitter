#ifndef SQ_TEST_SUPERTYPE_FIXTURE_H
#define SQ_TEST_SUPERTYPE_FIXTURE_H
// Tiny compiled tables whose reductions permit every supertype to wrap every
// other one. The analysis must compute the full power set, including cycles.
typedef struct {
  TSLanguage language;
  TSSymbolMetadata metadata[70];
  TSSymbol public_symbols[70], aliases[1];
  uint16_t table[3 * 70];
  TSParseActionEntry actions[70];
  TSLexerMode lex_modes[3];
} SupertypeFixture;
static void supertype_fixture(SupertypeFixture *f, unsigned count, bool connected) {
  memset(f, 0, sizeof(*f));
  uint32_t symbols = count + 2;
  assert(symbols <= 70);
  f->language = (TSLanguage){.abi_version = TREE_SITTER_LANGUAGE_VERSION,
      .symbol_count = symbols, .token_count = 2, .state_count = 3, .large_state_count = 3,
      .symbol_metadata = f->metadata, .public_symbol_map = f->public_symbols,
      .alias_map = f->aliases, .parse_table = f->table, .parse_actions = f->actions, .lex_modes = f->lex_modes};
  f->metadata[1].visible = f->metadata[1].named = true;
  for (uint32_t i = 0; i < symbols; i++) {
    f->public_symbols[i] = (TSSymbol)i;
    f->metadata[i].supertype = i >= 2;
    if (i >= 2 && connected) f->table[symbols + i] = 2;
  }
  if (connected) {
    f->table[2 * symbols] = 1;
    f->actions[1].entry.count = (uint8_t)count;
    for (unsigned i = 0; i < count; i++) {
      f->actions[i + 2].action = (TSParseAction){.reduce = {
          .type = TSParseActionTypeReduce, .symbol = (TSSymbol)(i + 2), .child_count = 1}};
    }
  }
}
#endif
