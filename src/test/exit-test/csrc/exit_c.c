#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void mark(const char *s) {
    write(1, s, strlen(s));
    write(1, "\n", 1);
}

static void on_atexit1(void) { mark("atexit1"); }
static void on_atexit2(void) { mark("atexit2"); }
static void on_quick_exit(void) { mark("at_quick_exit"); }

static void *exit_from_thread(void *arg) {
    exit(3);
}

static void *exit0_from_thread(void *arg) {
    exit(0);
}

static void *return_from_thread(void *arg) {
    mark("thread-returned");
    return NULL;
}

static void *pthread_exit_from_thread(void *arg) {
    mark("thread-pthread-exit");
    pthread_exit(NULL);
}

int main(int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : "return";
    atexit(on_atexit1);
    atexit(on_atexit2);
    at_quick_exit(on_quick_exit);
    // stdout is a file, so this stays buffered until stdio is flushed.
    printf("stdio-buffered");
    mark("main");
    if (!strcmp(mode, "exit"))
        exit(3);
    if (!strcmp(mode, "_exit"))
        _exit(3);
    if (!strcmp(mode, "quick_exit"))
        quick_exit(3);
    if (!strcmp(mode, "abort"))
        abort();
    if (!strcmp(mode, "thread-exit")) {
        pthread_t t;
        pthread_create(&t, NULL, exit_from_thread, NULL);
        pthread_join(t, NULL);
        return 99;
    }
    if (!strcmp(mode, "thread-exit0") || !strcmp(mode, "thread-finish")) {
        int finish = !strcmp(mode, "thread-finish");
        pthread_t t1, t2;
        pthread_create(&t1, NULL, finish ? return_from_thread : exit0_from_thread, NULL);
        if (finish)
            pthread_create(&t2, NULL, pthread_exit_from_thread, NULL);
        sleep(2);
        mark("main-woke");
        return finish ? 4 : 5;
    }
    if (!strcmp(mode, "pathconf")) {
        _exit(pathconf("/", _PC_PATH_MAX) > 0 && pathconf("/", _PC_NAME_MAX) > 0 ? 0 : 1);
    }
    return 3;
}
