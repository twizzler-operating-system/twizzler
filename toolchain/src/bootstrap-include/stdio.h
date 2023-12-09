#pragma once

#include <stddef.h>

#define fflush(...)
#ifndef fprintf
#define fprintf(...)
#endif 
#define stderr