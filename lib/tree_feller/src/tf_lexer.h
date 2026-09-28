// Lexer adapter: runs generated lexers and native scanners over UTF-8 input.
//
// A stripped-down `Lexer` (lexer.c): contiguous or callback UTF-8 input,
// no included ranges or incremental reuse. Keeps the observable behaviour the
// lexers, scanners, and byte/point arithmetic depend on.
#ifndef TF_LEXER_H
#define TF_LEXER_H

#include "tf_language.h"

#if defined(__GNUC__) || defined(__clang__)
#define TF_ALWAYS_INLINE inline __attribute__((always_inline))
#else
#define TF_ALWAYS_INLINE inline
#endif

// Shared by private replays; no borrowed chunk survives another reader's calls.
typedef struct {
  TFInput input;
  uint32_t size;
  bool has_size;
  bool overflow;
} TFInputState;

typedef struct {
  uint32_t length;
  char data[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
} TFScannerState;

typedef struct {
  void *payload;
  // The incoming buffer remains intact while scanning lookahead. Internal
  // tokens leave current and before pointing to the same snapshot.
  TFScannerState buffers[2];
  uint8_t current, before;
  bool token_external;
} TFScanner;

static inline TFScannerState *tf_scanner_state(TFScanner *self) {
  return &self->buffers[self->current];
}

static inline TFScannerState *tf_scanner_before(TFScanner *self) {
  return &self->buffers[self->before];
}

typedef struct {
  // First member: the generated lex functions are handed this pointer and cast
  // it back to the enclosing struct (parser.c:345).
  TSLexer data;

  const TFLanguage *lang;
  const uint8_t *source;
  uint32_t size;  // absolute end of the buffer or current chunk

  uint32_t byte;
  TFPoint point;  // like TFPoint everywhere else, `column` counts bytes
  uint32_t lookahead_size;

  uint32_t token_start_byte;
  TFPoint token_start_point;
  uint32_t token_end_byte;  // TF_NO_END until mark_end
  TFPoint token_end_point;

  // Whether the last token `tf_lexer_next` produced was reclassified by the
  // keyword lexer; the parser may have to undo that later, see
  // `tf_parser__demote_keyword`. Adding fields among the hot ones above moved
  // them across a cache line for a measured 10% loss.
  bool token_is_keyword;
  TSStateId token_lex_state;

  TFInputState *input;
  uint32_t chunk_start;
  bool at_eof;
  TFScanner *scanner;
} TFLexer;

// The source is one contiguous buffer, indexed directly.
void tf_lexer_init(TFLexer *self, const TFLanguage *lang, const void *source, uint32_t size);
void tf_lexer_init_with_callback(TFLexer *self, const TFLanguage *lang, TFInputState *input);

// Refetch after another lexer used the callback and invalidated this one's bytes.
void tf_lexer_refresh(TFLexer *self);

void tf_lexer_seek(TFLexer *self, uint32_t byte, TFPoint point);

// Lex the token that follows, in the given parse state, skipping any leading
// `extra` characters. Returns false if no token matches, leaving the position at
// the offending character for the caller to report.
bool tf_lexer_next(TFLexer *self, TSStateId state, TFToken *out);
// For grammars without external tokens; excludes scanner dispatch and snapshots.
bool tf_lexer_next_internal(TFLexer *self, TSStateId state, TFToken *out);

#endif  // TF_LEXER_H
