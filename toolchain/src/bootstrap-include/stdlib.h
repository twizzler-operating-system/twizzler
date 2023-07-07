#pragma once

_Noreturn static inline void abort(void) {
    __builtin_trap();
}

#define alloca __builtin_alloca