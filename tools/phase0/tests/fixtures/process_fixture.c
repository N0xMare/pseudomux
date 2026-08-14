#define _POSIX_C_SOURCE 200809L

#include <ctype.h>
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <unistd.h>

static const char *base_name(const char *path) {
    const char *slash = strrchr(path, '/');
    return slash == NULL ? path : slash + 1;
}

static int write_forever(void) {
    char payload[65536];
    memset(payload, 'x', sizeof(payload));
    for (;;) {
        ssize_t written = write(STDOUT_FILENO, payload, sizeof(payload));
        if (written < 0) {
            if (errno == EINTR) {
                continue;
            }
            return 0;
        }
    }
}

static int fixture_mode(const char *mode) {
    if (strcmp(mode, "sleep") == 0) {
        sleep(30);
        return 0;
    }
    if (strcmp(mode, "flood") == 0) {
        return write_forever();
    }
    if (strcmp(mode, "pipe-holder") == 0) {
        pid_t child = fork();
        if (child < 0) {
            return 70;
        }
        if (child == 0) {
            sleep(30);
            _exit(0);
        }
        _exit(0);
    }
    if (strcmp(mode, "escaped-descendant") == 0) {
        pid_t child = fork();
        if (child < 0) {
            return 71;
        }
        if (child == 0) {
            pid_t grandchild = fork();
            if (grandchild < 0) {
                _exit(72);
            }
            if (grandchild == 0) {
                if (setsid() < 0) {
                    _exit(73);
                }
                sleep(30);
                _exit(0);
            }
            _exit(0);
        }
        _exit(0);
    }
    return 64;
}

static int dispatch_python(int argc, char **argv) {
    const char *python = getenv("PMUX_PHASE0_FIXTURE_PYTHON");
    if (python == NULL || python[0] != '/') {
        return 78;
    }
    char variable[256] = "PMUX_PHASE0_SCRIPT_";
    size_t offset = strlen(variable);
    const char *name = base_name(argv[0]);
    for (size_t index = 0; name[index] != '\0' && offset + 1 < sizeof(variable);
         index++) {
        unsigned char value = (unsigned char)name[index];
        variable[offset++] = isalnum(value) ? (char)toupper(value) : '_';
    }
    variable[offset] = '\0';
    const char *script = getenv(variable);
    if (script == NULL || script[0] != '/') {
        return 79;
    }
    char **arguments = calloc((size_t)argc + 2, sizeof(char *));
    if (arguments == NULL) {
        return 80;
    }
    arguments[0] = (char *)python;
    arguments[1] = (char *)script;
    for (int index = 1; index < argc; index++) {
        arguments[index + 1] = argv[index];
    }
    arguments[argc + 1] = NULL;
    execv(python, arguments);
    return 81;
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "--fixture-mode") == 0) {
        return fixture_mode(argv[2]);
    }
    return dispatch_python(argc, argv);
}
