#include <dlfcn.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef size_t (*heap_objects_fn)(uint64_t *, size_t);
typedef void (*stats_fn)(uint64_t *);

// Heap spans the runtime's main talc holds, or -1 if the runtime does not say.
static long spans(void) {
    static uint64_t buf[3 * 1024 + 2];
    heap_objects_fn f = (heap_objects_fn)dlsym(RTLD_DEFAULT, "__twz_rt_diag_heap_objects");
    if (!f)
        return -1;
    size_t w = f(buf, sizeof(buf) / sizeof(buf[0]));
    return w >= 2 ? (long)buf[w - 2] : -1;
}

// What the runtime's allocator holds from its base (objects and heap spans), in MiB.
static long base_mib(void) {
    uint64_t st[8];
    stats_fn f = (stats_fn)dlsym(RTLD_DEFAULT, "__twz_rt_diag_decommit_stats");
    if (!f)
        return -1;
    f(st);
    return (long)((st[6] - st[7]) >> 20);
}

static void *worker(void *arg) {
    for (size_t n = 16; n <= 64 << 10; n *= 2)
        free(malloc(n));
    return arg;
}

static void touch(char *p, size_t len) {
    for (size_t off = 0; off < len; off += 1 << 20)
        p[off] = 1;
    p[len - 1] = 1;
}

// Fails if the heap kept growing after the first rounds warmed it up.
static int report(const char *mode, int i, long spans_before, long base_warm) {
    long now = spans(), base = base_mib();
    printf("heap-test: %s: %d rounds, spans %ld -> %ld, base %ld -> %ld MiB\n", mode, i,
           spans_before, now, base_warm, base);
    return now > spans_before + 2 || base > base_warm + 256;
}

int main(int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : "threads";
    int rounds = argc > 2 ? atoi(argv[2]) : 2048;
    long before = spans(), base_warm = -1;
    for (int i = 0; i < rounds; i++) {
        char *p;
        size_t len = (size_t)32 << 20;
        if (strcmp(mode, "aligned") == 0) {
            p = aligned_alloc(1 << 20, len);
        } else if (strcmp(mode, "malloc") == 0) {
            p = malloc(len);
        } else if (strcmp(mode, "sizes") == 0) {
            len = ((size_t)(i * 7919 % 61) + 3) << 20;
            p = malloc(len);
        } else if (strcmp(mode, "realloc") == 0) {
            p = NULL;
            for (size_t n = 1 << 20; n <= len; n *= 2) {
                p = realloc(p, n);
                if (!p)
                    break;
            }
        } else if (strcmp(mode, "threads") == 0) {
            pthread_t t[4];
            for (int k = 0; k < 4; k++)
                pthread_create(&t[k], NULL, worker, NULL);
            for (int k = 0; k < 4; k++)
                pthread_join(t[k], NULL);
            p = malloc(len);
        } else {
            printf("heap-test: unknown mode %s\n", mode);
            return 2;
        }
        if (!p) {
            printf("heap-test: %s: allocation %d of %zu bytes failed\n", mode, i, len);
            report(mode, i, before, base_warm);
            return 1;
        }
        touch(p, len);
        free(p);
        if (i == 16)
            base_warm = base_mib();
        if (i % 256 == 0)
            printf("heap-test: %s: round %d, spans %ld, base %ld MiB\n", mode, i, spans(),
                   base_mib());
    }
    return report(mode, rounds, before, base_warm);
}
