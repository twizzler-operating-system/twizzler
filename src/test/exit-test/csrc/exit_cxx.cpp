#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <unistd.h>

static void mark(const char *s) {
    write(1, s, strlen(s));
    write(1, "\n", 1);
}

struct Marker {
    const char *name;
    ~Marker() { mark(name); }
};

static Marker static_marker{"static-dtor"};
static thread_local Marker tls_marker{"tls-dtor"};

static void on_atexit() { mark("atexit"); }

int main(int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : "return";
    if (!strcmp(mode, "current_path")) {
        try {
            std::filesystem::current_path();
        } catch (...) {
            _exit(1);
        }
        _exit(0);
    }
    atexit(on_atexit);
    static Marker local_marker{"local-static-dtor"};
    mark(tls_marker.name ? "main" : "");
    printf("stdio-buffered");
    if (!strcmp(mode, "exit"))
        exit(3);
    if (!strcmp(mode, "abort"))
        abort();
    return 3;
}
