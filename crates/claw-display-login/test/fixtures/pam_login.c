/* SPDX-License-Identifier: Apache-2.0 */
#define _GNU_SOURCE
#include <security/pam_appl.h>
#include <errno.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int conversation(int count, const struct pam_message **messages,
                        struct pam_response **response, void *context) {
    if (count < 1 || count > 8)
        return PAM_CONV_ERR;
    struct pam_response *answers = calloc((size_t)count, sizeof(*answers));
    if (!answers)
        return PAM_BUF_ERR;
    for (int index = 0; index < count; index++) {
        if (messages[index]->msg_style != PAM_PROMPT_ECHO_OFF) {
            for (int previous = 0; previous < index; previous++)
                free(answers[previous].resp);
            free(answers);
            return PAM_CONV_ERR;
        }
        answers[index].resp = strdup((const char *)context);
        if (!answers[index].resp) {
            for (int previous = 0; previous < index; previous++)
                free(answers[previous].resp);
            free(answers);
            return PAM_BUF_ERR;
        }
    }
    *response = answers;
    return PAM_SUCCESS;
}

static int kernel_number(const char *path, unsigned *number) {
    FILE *file = fopen(path, "re");
    if (!file)
        return -1;
    int fields = fscanf(file, "%u", number);
    int closed = fclose(file);
    return fields == 1 && closed == 0 ? 0 : -1;
}

#include "gui_fixture.h"

static int display_hook(pam_handle_t *handle, int opening) {
    void *module = dlopen("/usr/lib/x86_64-linux-gnu/security/pam_claw_display.so",
                          RTLD_NOW | RTLD_LOCAL);
    if (!module)
        return PAM_SYSTEM_ERR;
    typedef int (*session_hook)(pam_handle_t *, int, int, const char **);
    void *symbol = dlsym(module, opening ? "pam_sm_open_session" : "pam_sm_close_session");
    session_hook hook;
    _Static_assert(sizeof(hook) == sizeof(symbol), "POSIX function pointer size");
    memcpy(&hook, &symbol, sizeof(hook));
    const char *phase = opening ? "open-after" : "close-before";
    int result = hook ? hook(handle, PAM_SILENT, 1, &phase) : PAM_SYSTEM_ERR;
    if (dlclose(module) != 0)
        return PAM_SYSTEM_ERR;
    return result;
}

int main(int argc, char **argv) {
    if (argc > 2 && strcmp(argv[1], "--entry") == 0)
        return gui_entry(argc - 2, argv + 2);
    if (getuid() != 0 || geteuid() != 0 || argc != 3)
        return 64;
    struct pam_conv conv = {
        .conv = conversation,
        .appdata_ptr = strcmp(argv[2], "deny-password") == 0
            ? "intentionally-wrong-fixture-password"
            : "private-disposable-display-fixture-password"
    };
    pam_handle_t *handle = NULL;
    int result = pam_start_confdir("cosmic-greeter", "claw-display-test",
                                  &conv, argv[1], &handle);
    if (result == PAM_SUCCESS)
        result = pam_authenticate(handle, PAM_SILENT);
    if (result == PAM_SUCCESS)
        result = pam_acct_mgmt(handle, PAM_SILENT);
    if (result == PAM_SUCCESS)
        result = pam_setcred(handle, PAM_ESTABLISH_CRED);
    if (result == PAM_SUCCESS)
        result = pam_putenv(handle, strcmp(argv[2], "invalid-locale") == 0
            ? "LANG=../must-not-be-a-locale" : "LANG=fr_CA.UTF-8");
    if (result == PAM_SUCCESS)
        result = pam_putenv(handle, "LC_TIME=C.UTF-8");
    if (result == PAM_SUCCESS)
        result = pam_putenv(handle, "LANGUAGE=fr:en");
    if (result == PAM_SUCCESS && strcmp(argv[2], "non-root-open") == 0) {
        if (setgroups(0, NULL) != 0 || setgid(62050) != 0 || setuid(62050) != 0)
            result = PAM_SYSTEM_ERR;
    }
    if (result == PAM_SUCCESS)
        result = pam_open_session(handle, PAM_SILENT);
    if (result != PAM_SUCCESS) {
        fprintf(stderr, "private PAM fixture refused: %s\n", pam_strerror(handle, result));
        if (handle)
            pam_end(handle, result);
        return 1;
    }
    unsigned owner, session;
    if (kernel_number("/proc/self/loginuid", &owner) != 0
        || kernel_number("/proc/self/sessionid", &session) != 0
        || owner != 62050 || session == 0xffffffffu) {
        fprintf(stderr, "PAM did not establish the actual kernel login identity\n");
        pam_close_session(handle, PAM_SILENT);
        pam_end(handle, PAM_SESSION_ERR);
        return 2;
    }
    printf("authenticated %u %u\n", owner, session);
    fflush(stdout);
    int fixture_result = 0;
    if (strcmp(argv[2], "duplicate-open") == 0) {
        if (display_hook(handle, 1) != PAM_SESSION_ERR) {
            fprintf(stderr, "duplicate private display activation was accepted\n");
            pam_close_session(handle, PAM_SILENT);
            pam_end(handle, PAM_SESSION_ERR);
            return 5;
        }
        printf("duplicate activation refused\n");
    } else if (strcmp(argv[2], "inherited-handle") == 0) {
        pid_t child = fork();
        if (child == 0) {
            if (setgroups(0, NULL) != 0 || setgid(62050) != 0 || setuid(62050) != 0)
                _exit(70);
            int closed = display_hook(handle, 0);
            pam_end(handle, closed);
            _exit(closed == PAM_SESSION_ERR ? 0 : 71);
        }
        int status;
        if (child < 0 || waitpid(child, &status, 0) != child
            || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            pam_close_session(handle, PAM_SILENT);
            pam_end(handle, PAM_SESSION_ERR);
            return 6;
        }
        printf("inherited PAM handle cannot retire its Root parent\n");
    } else if (strcmp(argv[2], "subscribe-reuse") == 0) {
        for (unsigned index = 0; index < 40; index++) {
            pid_t child = fork();
            if (child == 0) {
                if (setgroups(0, NULL) != 0 || setgid(62050) != 0 || setuid(62050) != 0
                    || syscall(SYS_close_range, 3u, ~0u, 0u) != 0 || clearenv() != 0)
                    _exit(72);
                execl("/usr/local/bin/claw-display-session", "claw-display-session",
                      "--exec", "/usr/bin/true", (char *)NULL);
                _exit(73);
            }
            int status;
            if (child < 0 || waitpid(child, &status, 0) != child
                || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
                pam_close_session(handle, PAM_SILENT);
                pam_end(handle, PAM_SESSION_ERR);
                return 7;
            }
        }
        printf("40 completed display subscribers release their slots\n");
    } else if (strcmp(argv[2], "session-startup-failure") == 0) {
        fixture_result = session_startup_failure();
    } else if (strcmp(argv[2], "gui") == 0) {
        if (gui_fixture() != 0) {
            pam_close_session(handle, PAM_SILENT);
            pam_end(handle, PAM_SESSION_ERR);
            return 3;
        }
    } else if (strcmp(argv[2], "authenticate-only") != 0) {
        char command[16];
        if (!fgets(command, sizeof(command), stdin) || strcmp(command, "close\n") != 0) {
            fprintf(stderr, "private PAM fixture lost its test controller\n");
            pam_close_session(handle, PAM_SILENT);
            pam_end(handle, PAM_SESSION_ERR);
            return 3;
        }
    }
    result = pam_close_session(handle, PAM_SILENT);
    if (result == PAM_SUCCESS)
        result = pam_setcred(handle, PAM_DELETE_CRED);
    if (result != PAM_SUCCESS)
        fprintf(stderr, "private PAM fixture retirement failed: %s\n", pam_strerror(handle, result));
    int ended = pam_end(handle, result);
    if (strcmp(argv[2], "session-startup-failure") == 0
        && result == PAM_SUCCESS && ended == PAM_SUCCESS)
        fprintf(stderr, "private PAM retired after cosmic-session startup failure\n");
    if (fixture_result != 0)
        return 8;
    return result == PAM_SUCCESS && ended == PAM_SUCCESS ? 0 : 4;
}
