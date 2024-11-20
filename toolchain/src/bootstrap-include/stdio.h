#pragma once

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define fflush(...)

extern int printf(const char *fmt, ...);
extern int fprintf(void *f, const char *fmt, ...);

#define stderr NULL

#ifdef __cplusplus
}
#endif
