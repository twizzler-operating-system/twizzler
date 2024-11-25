#pragma once

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define alloca __builtin_alloca

_Noreturn static inline void abort(void) {
    __builtin_trap();
}

extern void *malloc(size_t len);
extern char *getenv(const char *name);
extern void free(void *ptr);

#ifdef __cplusplus
}
#endif
