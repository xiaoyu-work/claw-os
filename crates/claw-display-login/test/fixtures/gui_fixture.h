/* SPDX-License-Identifier: Apache-2.0 */
#include <fcntl.h>
#include <dirent.h>
#include <grp.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>

static int gui_command(char command[4]) {
    struct ucred peer;
    socklen_t size = sizeof(peer);
    if (getsockopt(STDIN_FILENO, SOL_SOCKET, SO_PEERCRED, &peer, &size) != 0
        || size != sizeof(peer) || peer.uid != 0 || peer.pid <= 0)
        return -1;
    int enabled = 1;
    if (setsockopt(STDIN_FILENO, SOL_SOCKET, SO_PASSCRED, &enabled, sizeof(enabled)) != 0)
        return -1;
    struct pollfd ready = {.fd = STDIN_FILENO, .events = POLLIN};
    if (poll(&ready, 1, 60000) <= 0)
        return -1;
    char bytes[5];
    union {
        struct cmsghdr aligned;
        char bytes[CMSG_SPACE(sizeof(struct ucred)) + CMSG_SPACE(sizeof(int))];
    } ancillary;
    struct iovec vector = {.iov_base = bytes, .iov_len = sizeof(bytes)};
    struct msghdr message = {
        .msg_iov = &vector, .msg_iovlen = 1,
        .msg_control = ancillary.bytes, .msg_controllen = sizeof(ancillary.bytes)
    };
    ssize_t received = recvmsg(STDIN_FILENO, &message, MSG_CMSG_CLOEXEC);
    int authentic = 0, invalid = received != 4 || message.msg_flags & (MSG_TRUNC | MSG_CTRUNC);
    for (struct cmsghdr *header = CMSG_FIRSTHDR(&message); header;
         header = CMSG_NXTHDR(&message, header)) {
        if (header->cmsg_level == SOL_SOCKET && header->cmsg_type == SCM_CREDENTIALS
            && header->cmsg_len == CMSG_LEN(sizeof(struct ucred))) {
            struct ucred sender;
            memcpy(&sender, CMSG_DATA(header), sizeof(sender));
            if (authentic || sender.pid != peer.pid || sender.uid != 0 || sender.gid != peer.gid)
                invalid = 1;
            authentic = 1;
        } else {
            invalid = 1;
            if (header->cmsg_level == SOL_SOCKET && header->cmsg_type == SCM_RIGHTS) {
                size_t count = (header->cmsg_len - CMSG_LEN(0)) / sizeof(int);
                for (size_t index = 0; index < count; index++) {
                    int fd;
                    memcpy(&fd, CMSG_DATA(header) + index * sizeof(fd), sizeof(fd));
                    close(fd);
                }
            }
        }
    }
    if (!authentic || invalid)
        return -1;
    memcpy(command, bytes, 4);
    return 0;
}

static int gui_reply(const char tag[4], int descriptor) {
    union { struct cmsghdr aligned; char bytes[CMSG_SPACE(sizeof(int))]; } ancillary;
    struct iovec vector = {.iov_base = (void *)tag, .iov_len = 4};
    struct msghdr message = {.msg_iov = &vector, .msg_iovlen = 1};
    if (descriptor >= 0) {
        message.msg_control = ancillary.bytes;
        message.msg_controllen = sizeof(ancillary.bytes);
        struct cmsghdr *header = CMSG_FIRSTHDR(&message);
        header->cmsg_level = SOL_SOCKET;
        header->cmsg_type = SCM_RIGHTS;
        header->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(header), &descriptor, sizeof(descriptor));
    }
    return sendmsg(STDIN_FILENO, &message, MSG_NOSIGNAL) == 4 ? 0 : -1;
}

static int gui_entry(int count, char **arguments) {
    if (getuid() != 62050 || geteuid() != 62050 || getgid() != 62050
        || count != 7 || strcmp(arguments[0], "/usr/local/bin/cos") != 0)
        return 64;
    const char *runtime = getenv("XDG_RUNTIME_DIR");
    const char *display = getenv("WAYLAND_DISPLAY");
    const char *bus = getenv("DBUS_SESSION_BUS_ADDRESS");
    if (!runtime || strcmp(runtime, "/run/user/62050") != 0
        || !display || strncmp(display, "wayland-", 8) != 0 || strchr(display, '/')
        || !bus || strcmp(bus, "unix:path=/run/user/62050/bus") != 0
        || getenv("WAYLAND_SOCKET") || getenv("DISPLAY") || getenv("XAUTHORITY")
        || getenv("CLAW_DISPLAY_CONTROL_FD") || getenv("COSMIC_SESSION_SOCK")
        || getenv("X_PRIVILEGED_WAYLAND_SOCKET"))
        return 65;
    char path[256];
    int length = snprintf(path, sizeof(path), "%s/%s", runtime, display);
    struct stat metadata;
    if (length < 0 || (size_t)length >= sizeof(path) || stat(path, &metadata) != 0
        || !S_ISSOCK(metadata.st_mode) || metadata.st_uid != 62050)
        return 66;
    DIR *descriptors = opendir("/proc/self/fd");
    if (!descriptors)
        return 67;
    struct dirent *entry;
    int leaked = 0;
    while ((entry = readdir(descriptors))) {
        unsigned descriptor;
        if (sscanf(entry->d_name, "%u", &descriptor) == 1
            && descriptor > 2 && descriptor != (unsigned)dirfd(descriptors))
            leaked = 1;
    }
    if (closedir(descriptors) != 0 || leaked)
        return 68;
    execv(arguments[0], arguments);
    return 69;
}

static int gui_host(void) {
    char path[128];
    snprintf(path, sizeof(path), "/proc/self/task/%d/children", getpid());
    FILE *children = fopen(path, "re");
    if (!children)
        return -1;
    int child;
    char extra;
    int count = fscanf(children, "%d %c", &child, &extra);
    int closed = fclose(children);
    if (count != 1 || closed != 0 || child <= 1)
        return -1;
    int descriptor = (int)syscall(SYS_pidfd_open, child, 0u);
    if (descriptor < 0)
        return -1;
    int result = gui_reply("HOST", descriptor);
    close(descriptor);
    return result;
}

static int gui_fixture(void) {
    static const char *modes[] = {
        "zero", "read", "write", "both", "revoke", "aliases",
        "revoke", "revoke", "revoke", "revoke", "revoke", "revoke"
    };
    pid_t child = -1;
    int gate = -1, result = -1;
    if (gui_reply("AUTH", -1) != 0)
        return -1;
    fprintf(stderr, "private PAM GUI controller ready\n");
    for (;;) {
        char command[4];
        if (gui_command(command) != 0)
            break;
        if (memcmp(command, "HOST", 4) == 0 && child < 0) {
            if (gui_host() != 0)
                break;
        } else if (memcmp(command, "CASE", 4) == 0 && child < 0) {
            char selected[4];
            if (gui_command(selected) != 0 || selected[0] < '0' || selected[0] > '9'
                || selected[1] < '0' || selected[1] > '9'
                || memcmp(selected + 2, "..", 2) != 0)
                break;
            unsigned scenario = (unsigned)(selected[0] - '0') * 10u
                + (unsigned)(selected[1] - '0');
            if (scenario >= sizeof(modes) / sizeof(modes[0]))
                break;
            int descriptors[2];
            if (pipe2(descriptors, O_CLOEXEC) != 0)
                break;
            child = fork();
            if (child == 0) {
                close(descriptors[1]);
                close(STDIN_FILENO);
                if (open("/dev/null", O_RDONLY) != STDIN_FILENO
                    || setgroups(0, NULL) != 0 || setgid(62050) != 0 || setuid(62050) != 0)
                    _exit(120);
                char release;
                if (read(descriptors[0], &release, 1) != 1 || release != 'G')
                    _exit(121);
                close(descriptors[0]);
                char session[64];
                int length = snprintf(session, sizeof(session), "gui-fixture-parent-%u", scenario);
                if (length < 0 || (size_t)length >= sizeof(session))
                    _exit(122);
                if (syscall(SYS_close_range, 3u, ~0u, 0u) != 0
                    || clearenv() != 0 || chdir("/run/display-home") != 0
                    || setenv("HOME", "/run/display-home", 1) != 0
                    || setenv("PATH", "/usr/local/bin:/usr/bin:/bin", 1) != 0
                    || setenv("LANG", "fr_CA.UTF-8", 1) != 0
                    || setenv("LANGUAGE", "fr:en", 1) != 0
                    || setenv("LC_TIME", "C.UTF-8", 1) != 0
                    || setenv("COS_SESSION", session, 1) != 0
                    || setenv("COS_PROC_DATA_DIR", "/run/cos/caps/62050", 1) != 0
                    || setenv("XDG_RUNTIME_DIR", "/run/forged-runtime", 1) != 0
                    || setenv("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/forged-bus", 1) != 0
                    || setenv("WAYLAND_DISPLAY", "/run/forged-display", 1) != 0
                    || setenv("WAYLAND_SOCKET", "99", 1) != 0
                    || setenv("DISPLAY", ":99", 1) != 0
                    || setenv("XAUTHORITY", "/run/forged-authority", 1) != 0
                    || setenv("X_PRIVILEGED_WAYLAND_SOCKET", "98", 1) != 0
                    || setenv("COSMIC_SESSION_SOCK", "/run/forged-session", 1) != 0
                    || setenv("CLAW_DISPLAY_CONTROL_FD", "97", 1) != 0)
                    _exit(122);
                execl("/usr/local/bin/claw-display-session", "claw-display-session", "--exec",
                      "/usr/lib/cos/bin/pam-login-fixture", "--entry", "/usr/local/bin/cos", "app",
                      scenario == 0 ? "gui-boundary-zero" : "gui-boundary-test", "surface",
                      modes[scenario], "literal ; quote' --flag", "", (char *)NULL);
                _exit(123);
            }
            close(descriptors[0]);
            if (child < 0) {
                close(descriptors[1]);
                break;
            }
            gate = descriptors[1];
            int pidfd = (int)syscall(SYS_pidfd_open, child, 0u);
            if (pidfd < 0 || gui_reply("CHLD", pidfd) != 0) {
                if (pidfd >= 0) close(pidfd);
                break;
            }
            close(pidfd);
            fprintf(stderr, "private PAM GUI launcher staged\n");
        } else if (memcmp(command, "RUN!", 4) == 0 && child > 0 && gate >= 0) {
            if (write(gate, "G", 1) != 1)
                break;
            close(gate);
            gate = -1;
            fprintf(stderr, "private PAM GUI launcher released\n");
        } else if (memcmp(command, "WAIT", 4) == 0 && child > 0 && gate < 0) {
            int status;
            if (waitpid(child, &status, 0) != child)
                break;
            child = -1;
            if (gui_reply(WIFEXITED(status) && WEXITSTATUS(status) == 0 ? "PASS" : "FAIL", -1) != 0)
                break;
        } else if (memcmp(command, "CLSE", 4) == 0 && child < 0) {
            result = 0;
            break;
        } else {
            break;
        }
    }
    if (gate >= 0) close(gate);
    if (child > 0) {
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
    }
    if (result != 0)
        fprintf(stderr, "private GUI PAM controller failed\n");
    return result;
}

static pid_t session_child(char *const arguments[], int output) {
    pid_t child = fork();
    if (child == 0) {
        if ((output >= 0 && dup2(output, STDOUT_FILENO) != STDOUT_FILENO)
            || setgroups(0, NULL) != 0 || setgid(62050) != 0 || setuid(62050) != 0
            || syscall(SYS_close_range, 3u, ~0u, 0u) != 0 || clearenv() != 0
            || chdir("/run/display-home") != 0
            || setenv("HOME", "/run/display-home", 1) != 0
            || setenv("PATH", "/run/gui-session-empty-path", 1) != 0
            || setenv("XDG_RUNTIME_DIR", "/run/user/62050", 1) != 0
            || setenv("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/62050/bus", 1) != 0)
            _exit(120);
        execv(arguments[0], arguments);
        _exit(123);
    }
    return child;
}

static int session_ready(int descriptor, const char *prefix) {
    struct timespec started, now;
    if (clock_gettime(CLOCK_MONOTONIC, &started) != 0)
        return -1;
    char line[1024];
    size_t length = 0;
    while (length < sizeof(line) - 1) {
        if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
            return -1;
        long remaining = 5000 - (now.tv_sec - started.tv_sec) * 1000
            - (now.tv_nsec - started.tv_nsec) / 1000000;
        struct pollfd ready = {.fd = descriptor, .events = POLLIN};
        if (remaining <= 0 || poll(&ready, 1, (int)remaining) <= 0)
            return -1;
        ssize_t count = read(descriptor, line + length, sizeof(line) - 1 - length);
        if (count <= 0)
            return -1;
        length += (size_t)count;
        line[length] = '\0';
        if (strchr(line, '\n'))
            return strncmp(line, prefix, strlen(prefix)) == 0 ? 0 : -1;
    }
    return -1;
}

static int session_exited(pid_t child, int timeout) {
    int descriptor = (int)syscall(SYS_pidfd_open, child, 0u);
    if (descriptor < 0)
        return -1;
    struct pollfd ready = {.fd = descriptor, .events = POLLIN};
    int result = poll(&ready, 1, timeout);
    if (close(descriptor) != 0 || result < 0)
        return -1;
    return result == 0 ? 0 : (ready.revents & POLLIN ? 1 : -1);
}

static int session_cleanup(pid_t child) {
    if (child < 0)
        return 0;
    if (kill(child, SIGKILL) != 0 || session_exited(child, 5000) != 1
        || waitpid(child, NULL, 0) != child) {
        fprintf(stderr, "private session child cleanup failed: %d\n", child);
        return -1;
    }
    return 0;
}

static int session_startup_failure(void) {
    pid_t bus = -1, observer = -1, session = -1;
    int result = -1, descriptors[2];
    char *const bus_arguments[] = {
        "/usr/bin/dbus-daemon", "--session", "--nofork", "--nopidfile",
        "--address=unix:path=/run/user/62050/bus", "--print-address=1", NULL
    };
    if (pipe2(descriptors, O_CLOEXEC) != 0)
        goto cleanup;
    bus = session_child(bus_arguments, descriptors[1]);
    close(descriptors[1]);
    int ready = bus > 0
        ? session_ready(descriptors[0], "unix:path=/run/user/62050/bus") : -1;
    close(descriptors[0]);
    if (ready != 0) {
        fprintf(stderr, "private session bus did not become ready\n");
        goto cleanup;
    }
    char *const observer_arguments[] = {
        "/usr/local/bin/claw-display-session", "--watch", NULL
    };
    if (pipe2(descriptors, O_CLOEXEC) != 0)
        goto cleanup;
    observer = session_child(observer_arguments, descriptors[1]);
    close(descriptors[1]);
    ready = observer > 0 ? session_ready(descriptors[0], "{\"action\":\"ready\",") : -1;
    close(descriptors[0]);
    if (ready != 0) {
        fprintf(stderr, "private authenticated display observer did not become ready\n");
        goto cleanup;
    }
    fprintf(stderr, "private authenticated display observer is ready\n");
    char *const session_arguments[] = {"/usr/bin/cosmic-session", NULL};
    session = session_child(session_arguments, -1);
    if (session <= 0)
        goto cleanup;
    int exited = session_exited(session, 5000), status;
    if (exited == 1 && waitpid(session, &status, 0) == session) {
        session = -1;
        if (WIFEXITED(status) && WEXITSTATUS(status) == 101) {
            fprintf(stderr, "private cosmic-session exited with startup panic: 101\n");
            result = 0;
        } else {
            fprintf(stderr, "private cosmic-session returned an unexpected status: %d\n", status);
        }
    } else {
        fprintf(stderr, "private cosmic-session did not exit within 5s after startup failure\n");
    }
    if (session_exited(observer, 0) == 0 && session_exited(bus, 0) == 0) {
        fprintf(stderr, "private authenticated display subscription remained alive after startup failure\n");
    } else {
        fprintf(stderr, "private display subscription or session bus exited before observation\n");
        result = -1;
    }
cleanup:
    if (session > 0)
        fprintf(stderr, "private fixture must kill the non-exiting cosmic-session\n");
    if (session_cleanup(session) != 0)
        result = -1;
    if (session_cleanup(observer) != 0)
        result = -1;
    if (session_cleanup(bus) != 0)
        result = -1;
    return result;
}
