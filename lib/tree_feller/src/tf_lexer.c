// Implements TSLexer callbacks and token dispatch: external scanner, generated
// lex_fn, then keyword re-lexing. Ported from tree-sitter's lexer.c and parser.c; see
// tf_lexer.h for the supported input modes.
#include "tf_lexer.h"

#include "tf_utf8.h"
#include <assert.h>
#include <string.h>

#define TF_BOM 0xFEFF
#define TF_NO_END UINT32_MAX

static bool tf_lexer__eof(const TSLexer *lexer) {
  const TFLexer *self = (const TFLexer *)lexer;
  return self->byte >= self->size;
}

// lexer.c:102-136, minus the "chunk ended mid-character" retry, which cannot
// happen when the whole input is one chunk.
static void tf_lexer__get_lookahead(TFLexer *self) {
  if (self->byte >= self->size) {
    self->lookahead_size = 1;
    self->data.lookahead = '\0';
    return;
  }
  // ASCII, which is nearly every byte, without the call: the decoder is too
  // large for the compiler to inline here, and it was showing up as its own
  // frame in a profile of this loop.
  uint8_t lead = self->source[self->byte];
  if (lead < 0x80) {
    self->lookahead_size = 1;
    self->data.lookahead = lead;
    return;
  }
  uint32_t i = 1;
  self->data.lookahead = tf_utf8_next(self->source + self->byte, self->size - self->byte, &i);
  // A malformed sequence advances one byte, as tree-sitter does (lexer.c:130).
  self->lookahead_size = (self->data.lookahead == TF_DECODE_ERROR) ? 1 : i;
}

static void tf_lexer__read(TFLexer *self, uint32_t byte, TFPoint point) {
  TFInputState *input = self->input;
  self->chunk_start = self->size = byte;
  self->source = NULL;
  if (input->overflow || (input->has_size && byte >= input->size)) return;

  uint32_t size = 0;
  const char *source = input->input.read(input->input.payload, byte, point, &size);
  if (size > UINT32_MAX - byte) {
    input->overflow = true;
  } else if (!size) {
    input->has_size = true;
    input->size = byte;
  } else {
    self->source = (const uint8_t *)source;
    self->size = byte + size;
  }
}

static void tf_lexer__get_chunk_lookahead(TFLexer *self) {
  if (!self->source || self->byte < self->chunk_start || self->byte >= self->size) {
    tf_lexer__read(self, self->byte, self->point);
  }
  self->at_eof = !self->source;
  self->lookahead_size = 1;
  if (self->at_eof) {
    self->data.lookahead = '\0';
    return;
  }

  const uint8_t *source = self->source + (self->byte - self->chunk_start);
  uint8_t lead = source[0];
  if (lead < 0x80) {
    self->data.lookahead = lead;
    return;
  }
  uint32_t available = self->size - self->byte;
  uint32_t needed = lead < 0xC2 || lead >= 0xF5 ? 1 : lead < 0xE0 ? 2 : lead < 0xF0 ? 3 : 4;
  uint8_t joined[4];
  if (available < needed) {
    // Copy before calling read: the provider may replace or overwrite its buffer.
    memcpy(joined, source, available);
    TFPoint point = self->point;
    uint32_t scanned = 0;
    while (available < needed) {
      while (scanned < available) {
        if (joined[scanned++] == '\n') {
          point.row++;
          point.column = 0;
        } else {
          point.column++;
        }
      }
      tf_lexer__read(self, self->byte + available, point);
      if (!self->source) break;
      uint32_t count = self->size - self->chunk_start;
      if (count > needed - available) count = needed - available;
      memcpy(joined + available, self->source, count);
      available += count;
    }
    source = joined;
  }
  self->data.lookahead = tf_utf8_next(source, available, &self->lookahead_size);
  if (self->data.lookahead == TF_DECODE_ERROR) self->lookahead_size = 1;
}

void tf_lexer_seek(TFLexer *self, uint32_t byte, TFPoint point) {
  self->byte = byte;
  self->point = point;
  if (self->input) tf_lexer__get_chunk_lookahead(self);
  else tf_lexer__get_lookahead(self);
}

void tf_lexer_refresh(TFLexer *self) {
  if (self->input) {
    self->source = NULL;
    tf_lexer__get_chunk_lookahead(self);
  }
}

// lexer.c:194-247. The included-range walk collapses to nothing with one range;
// what remains is the position arithmetic, which must match exactly: only '\n'
// advances the row, and `column` counts bytes.
static void tf_lexer__move(TFLexer *self, bool skip) {
  if (self->lookahead_size) {
    if (self->data.lookahead == '\n') {
      self->point.row++;
      self->point.column = 0;
    } else {
      self->point.column += self->lookahead_size;
    }
    self->byte += self->lookahead_size;
  }
  if (skip) {
    self->token_start_byte = self->byte;
    self->token_start_point = self->point;
  }
}

static void tf_lexer__advance(TSLexer *lexer, bool skip) {
  TFLexer *self = (TFLexer *)lexer;
  if (self->byte >= self->size) {
    return;  // lexer.c:250, `if (!self->chunk) return`
  }
  tf_lexer__move(self, skip);
  tf_lexer__get_lookahead(self);
}

static void tf_lexer__advance_chunk(TSLexer *lexer, bool skip) {
  TFLexer *self = (TFLexer *)lexer;
  if (self->at_eof) return;
  tf_lexer__move(self, skip);
  uint32_t offset = self->byte - self->chunk_start;
  if (offset < self->size - self->chunk_start) {
    uint8_t lead = self->source[offset];
    if (lead < 0x80) {
      self->lookahead_size = 1;
      self->data.lookahead = lead;
      return;
    }
  }
  tf_lexer__get_chunk_lookahead(self);
}

static bool tf_lexer__chunk_eof(const TSLexer *lexer) {
  return ((const TFLexer *)lexer)->at_eof;
}

static void tf_lexer__mark_end(TSLexer *lexer) {
  TFLexer *self = (TFLexer *)lexer;
  self->token_end_byte = self->byte;
  self->token_end_point = self->point;
}

// lexer.c:285-322. Unlike TFPoint.column this counts codepoints, and skips a
// byte order mark at offset 0.
//
// CONSIDERATION: rescans the line on every call, where tree-sitter caches the
// running count. No grammar in scope calls get_column at all; cache it if one
// does and the O(line length) per call shows up.
static uint32_t tf_lexer__get_column(TSLexer *lexer) {
  TFLexer *self = (TFLexer *)lexer;
  uint32_t column = 0;
  for (uint32_t i = self->byte - self->point.column; i < self->byte;) {
    uint32_t next = 1;
    int32_t code_point = tf_utf8_next(self->source + i, self->size - i, &next);
    if (code_point == TF_DECODE_ERROR) {
      next = 1;
    }
    if (i != 0 || code_point != TF_BOM) {
      column++;
    }
    i += next;
  }
  return column;
}

static uint32_t tf_lexer__get_chunk_column(TSLexer *lexer) {
  TFLexer *self = (TFLexer *)lexer;
  TFLexer scan = *self;
  tf_lexer_seek(&scan, self->byte - self->point.column, (TFPoint){self->point.row, 0});
  uint32_t column = 0;
  while (scan.byte < self->byte && !scan.at_eof) {
    if (scan.byte != 0 || scan.data.lookahead != TF_BOM) column++;
    tf_lexer__advance_chunk(&scan.data, false);
  }
  tf_lexer_refresh(self);
  return column;
}

// lexer.c:325-333. True only at the very start of the one included range.
static bool tf_lexer__is_at_included_range_start(const TSLexer *lexer) {
  const TFLexer *self = (const TFLexer *)lexer;
  return self->byte == 0 && self->size > 0;
}

static void tf_lexer__log(const TSLexer *lexer, const char *format, ...) {
  (void)lexer;
  (void)format;
}

void tf_lexer_init(TFLexer *self, const TFLanguage *lang, const void *source, uint32_t size) {
  *self = (TFLexer){
      .data =
          {
              .advance = tf_lexer__advance,
              .mark_end = tf_lexer__mark_end,
              .get_column = tf_lexer__get_column,
              .is_at_included_range_start = tf_lexer__is_at_included_range_start,
              .eof = tf_lexer__eof,
              .log = tf_lexer__log,
          },
      .lang = lang,
      .source = source,
      .size = size,
  };
  tf_lexer__get_lookahead(self);
}

void tf_lexer_init_with_callback(TFLexer *self, const TFLanguage *lang, TFInputState *input) {
  tf_lexer_init(self, lang, NULL, 0);
  self->input = input;
  self->data.advance = tf_lexer__advance_chunk;
  self->data.eof = tf_lexer__chunk_eof;
  self->data.get_column = tf_lexer__get_chunk_column;
  tf_lexer__get_chunk_lookahead(self);
}

// lexer.c:/ts_lexer_start/. tree-sitter decodes only when its lookahead was
// invalidated by moving between input chunks or included ranges. Here every
// seek, advance, and chunk refresh decodes the lookahead. Avoids one decode per
// token and another on every keyword re-lex.
static void tf_lexer__start(TFLexer *self) {
  self->token_start_byte = self->byte;
  self->token_start_point = self->point;
  self->token_end_byte = TF_NO_END;
  self->data.result_symbol = 0;
  if (self->byte == 0 && self->size > 0 && self->data.lookahead == TF_BOM) {
    self->data.advance(&self->data, true);
  }
}

static void tf_lexer__finish(TFLexer *self) {
  if (self->token_end_byte == TF_NO_END) {
    tf_lexer__mark_end(&self->data);
  }
  // Scanners can mark an empty token before skipping its lookahead whitespace.
  if (self->token_end_byte < self->token_start_byte) {
    self->token_start_byte = self->token_end_byte;
    self->token_start_point = self->token_end_point;
  }
}

static bool tf_lexer__scan(TFLexer *self, TSStateId state, bool error_mode) {
  const TSLanguage *ts = self->lang->ts;
  TSLexerMode mode = tf_lex_mode(self->lang, state);
  uint32_t byte = self->byte;
  TFPoint point = self->point;
  TFScanner *scanner = self->scanner;
  if (mode.external_lex_state && scanner) {
    tf_lexer__start(self);
    ts->external_scanner.deserialize(scanner->payload, scanner->before.data,
                                     scanner->before.length);
    const bool *valid = ts->external_scanner.states +
                        (size_t)mode.external_lex_state * ts->external_token_count;
    bool found = ts->external_scanner.scan(scanner->payload, &self->data, valid);
    tf_lexer__finish(self);
    if (found) {
      scanner->state.length = ts->external_scanner.serialize(scanner->payload, scanner->state.data);
      assert(scanner->state.length <= TREE_SITTER_SERIALIZATION_BUFFER_SIZE);
      TSSymbol symbol = ts->external_scanner.symbol_map[self->data.result_symbol];
      bool changed = scanner->state.length != scanner->before.length ||
                     memcmp(scanner->state.data, scanner->before.data, scanner->state.length);
      // Empty extras must advance the scanner state; ordinary empty tokens can
      // advance the parse state (e.g. indentation).
      if (self->token_end_byte > byte || changed ||
          (!error_mode && tf_next_state(self->lang, state, symbol) != state)) {
        self->data.result_symbol = symbol;
        scanner->token_external = true;
        return true;
      }
    }
    scanner->state.length = scanner->before.length;
    memcpy(scanner->state.data, scanner->before.data, scanner->before.length);
    tf_lexer_seek(self, byte, point);
  }
  tf_lexer__start(self);
  bool found = ts->lex_fn(&self->data, mode.lex_state);
  tf_lexer__finish(self);
  return found;
}

bool tf_lexer_next(TFLexer *self, TSStateId state, TFToken *out) {
  const TSLanguage *ts = self->lang->ts;
  uint32_t start_byte = self->byte;
  TFPoint start_point = self->point;

  self->token_is_keyword = false;
  self->token_lex_state = state;
  if (self->scanner) {
    self->scanner->before.length = self->scanner->state.length;
    memcpy(self->scanner->before.data, self->scanner->state.data, self->scanner->state.length);
    self->scanner->token_external = false;
  }
  bool found = tf_lexer__scan(self, state, false);
  if (!found && state != 0) {
    // Mainline caches tokens from the error-state lexer even for failed GLR
    // branches. Surviving branches can reuse their different token boundaries.
    tf_lexer_seek(self, start_byte, start_point);
    found = tf_lexer__scan(self, 0, true);
  }
  if (!found) {
    return false;
  }

  out->symbol = self->data.result_symbol;
  out->start_byte = self->token_start_byte;
  out->start_point = self->token_start_point;
  out->end_byte = self->token_end_byte;
  out->end_point = self->token_end_point;

  // parser.c:645-670. A token that lexed as the word token is re-lexed with the
  // keyword lexer, and only reclassified if that lexer accepted, consumed exactly
  // the same bytes, and the resulting symbol is usable in this state -- either it
  // has actions, or the state reserves it. The keyword lexer is always called
  // with state 0.
  if (out->symbol == ts->keyword_capture_token && out->symbol != 0 &&
      (!self->scanner || !self->scanner->token_external)) {
    tf_lexer_seek(self, out->start_byte, out->start_point);
    tf_lexer__start(self);
    bool is_keyword = ts->keyword_lex_fn(&self->data, 0);
    tf_lexer__finish(self);
    if (is_keyword && self->token_end_byte == out->end_byte &&
        (tf_lookup(self->lang, state, self->data.result_symbol) != 0 ||
         tf_is_reserved_word(self->lang, state, self->data.result_symbol))) {
      out->symbol = self->data.result_symbol;
      // Recorded because the substitution was judged against *this* state, and
      // the parser may end up dispatching the token in a later one.
      self->token_is_keyword = true;
    }
  }

  // The keyword lexer may have stopped past the token, and the main lexer may
  // have read lookahead beyond it, so reposition explicitly. tree-sitter does the
  // same, from the parse stack's position (parser.c:531).
  tf_lexer_seek(self, out->end_byte, out->end_point);
  return true;
}
