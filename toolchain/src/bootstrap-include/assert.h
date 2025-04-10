#pragma once

#include<stdio.h>
#include<stdlib.h>

#ifdef DEBUG
#define assert(x) ((void)(!(x) && fprintf(stderr, "assertion failed: " #x " at %s:%d", __FILE__, __LINE__) && (abort(), 1)))
#else
#define assert(x) ((void)sizeof(x))
#endif


#define static_assert(...) 
