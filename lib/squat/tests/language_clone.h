#ifndef SQUAT_TEST_LANGUAGE_CLONE_H_
#define SQUAT_TEST_LANGUAGE_CLONE_H_

#include "../internal.h"
#include <stddef.h>

// Generated parsers use the TSLanguage size from their ABI version. Copying
// sizeof(TSLanguage) from an ABI-13/14 parser reads past its static object.
static TSLanguage test_clone_language(const TSLanguage *language) {
  TSLanguage result = {0};
  size_t bytes = offsetof(TSLanguage, primary_state_ids);
  if (language->abi_version >= LANGUAGE_VERSION_WITH_PRIMARY_STATES) {
    bytes = offsetof(TSLanguage, name);
  }
  if (language->abi_version >= LANGUAGE_VERSION_WITH_RESERVED_WORDS) {
    bytes = sizeof(result);
  }
  memcpy(&result, language, bytes);
  return result;
}

#endif
