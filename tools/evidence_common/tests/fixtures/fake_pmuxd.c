#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t stop_requested = 0;

static void request_stop(int signal_number) {
    (void)signal_number;
    stop_requested = 1;
}

static int write_pid(const char *path, pid_t pid) {
    int descriptor = open(path, O_WRONLY | O_CREAT | O_EXCL, 0600);
    char buffer[64];
    int length;
    if (descriptor < 0) {
        return -1;
    }
    length = snprintf(buffer, sizeof(buffer), "%ld\n", (long)pid);
    if (length < 1 || write(descriptor, buffer, (size_t)length) != length) {
        close(descriptor);
        return -1;
    }
    return close(descriptor);
}

static int spawn_escape(const char *pid_path) {
    pid_t first = fork();
    if (first < 0) {
        return -1;
    }
    if (first == 0) {
        pid_t second;
        if (setsid() < 0) {
            _exit(91);
        }
        second = fork();
        if (second < 0) {
            _exit(92);
        }
        if (second > 0) {
            _exit(0);
        }
        close(STDIN_FILENO);
        close(STDOUT_FILENO);
        close(STDERR_FILENO);
        if (write_pid(pid_path, getpid()) != 0) {
            _exit(93);
        }
        for (;;) {
            pause();
        }
    }
    while (waitpid(first, NULL, 0) < 0 && errno == EINTR) {
    }
    return 0;
}

int main(int argc, char **argv) {
    int index;
    int ignore_term = 0;
    int flood = 0;
    int exit_early = 0;
    int close_marker = -1;
    const char *escape_pid_path = NULL;
    const char *term_escape_pid_path = NULL;

    for (index = 1; index < argc; index++) {
        if (strcmp(argv[index], "--ignore-term") == 0) {
            ignore_term = 1;
        } else if (strcmp(argv[index], "--flood") == 0) {
            flood = 1;
        } else if (strcmp(argv[index], "--exit-early") == 0) {
            exit_early = 1;
        } else if (strcmp(argv[index], "--close-marker") == 0 && index + 1 < argc) {
            close_marker = atoi(argv[++index]);
        } else if (strcmp(argv[index], "--spawn-escape") == 0 && index + 1 < argc) {
            escape_pid_path = argv[++index];
        } else if (strcmp(argv[index], "--spawn-escape-on-term") == 0 && index + 1 < argc) {
            term_escape_pid_path = argv[++index];
        } else {
            fprintf(stderr, "unknown argument: %s\n", argv[index]);
            return 64;
        }
    }

    if (ignore_term) {
        signal(SIGTERM, SIG_IGN);
    } else {
        signal(SIGTERM, request_stop);
        signal(SIGUSR1, request_stop);
    }
    if (close_marker >= 0) {
        close(close_marker);
    }
    if (escape_pid_path != NULL && spawn_escape(escape_pid_path) != 0) {
        return 70;
    }
    if (exit_early) {
        puts("EARLY");
        fflush(stdout);
        return 3;
    }
    puts("READY");
    fflush(stdout);
    if (flood) {
        char block[4096];
        memset(block, 'x', sizeof(block));
        for (;;) {
            if (write(STDOUT_FILENO, block, sizeof(block)) < 0) {
                return 71;
            }
        }
    }
    while (!stop_requested) {
        usleep(10000);
    }
    if (term_escape_pid_path != NULL && spawn_escape(term_escape_pid_path) != 0) {
        return 72;
    }
    puts("STOP");
    fflush(stdout);
    return 0;
}
