#pragma once

#include <stddef.h>

#define fflush(...)
#ifndef fprintf
#define fprintf(...)
#endif 

extern int printf(const char *fmt, ...);

#define stderr
