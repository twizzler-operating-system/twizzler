#pragma once

_Noreturn static inline void abort(void) {
    __builtin_trap();
}

#define alloca __builtin_alloca

#include <stddef.h>
extern void *malloc(size_t len);
char *getenv(const char *name);
extern void free(void *ptr);
