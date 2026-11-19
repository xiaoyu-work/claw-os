#define _DEFAULT_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <security/pam_appl.h>
#include <security/pam_modules.h>
#include <shadow.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <unistd.h>

static const char pending[] = "/var/lib/cos/local-desktop-password.pending";

static int initial_password(void)
{
    struct spwd entry, *found;
    char storage[16384];
    int status = getspnam_r("cos", &entry, storage, sizeof(storage), &found);
    int initial = status || !found ? -1 : strcmp(found->sp_pwdp, "!") == 0;
    explicit_bzero(storage, sizeof(storage));
    return initial;
}

static int set_password(pam_handle_t *login)
{
    const void *conversation;
    int status = pam_get_item(login, PAM_CONV, &conversation);
    if (status != PAM_SUCCESS)
        return status;
    pam_handle_t *setup = NULL;
    status = pam_start("claw-first-password", "cos", conversation, &setup);
    if (status != PAM_SUCCESS)
        return status;
    status = pam_chauthtok(setup, 0);
    pam_end(setup, status);
    return status;
}

PAM_EXTERN int pam_sm_authenticate(pam_handle_t *login, int flags,
                                   int argc, const char **argv)
{
    (void)flags;
    (void)argc;
    (void)argv;
    const void *service;
    const char *user;
    if (getuid() != 0 || geteuid() != 0 ||
        pam_get_item(login, PAM_SERVICE, &service) != PAM_SUCCESS || !service ||
        strcmp(service, "claw-desktop") ||
        pam_get_user(login, &user, NULL) != PAM_SUCCESS || !user)
        return PAM_AUTH_ERR;
    if (strcmp(user, "cos"))
        return PAM_IGNORE;

    int fd = open(pending, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK);
    if (fd < 0)
        return errno == ENOENT ? PAM_IGNORE : PAM_AUTH_ERR;
    int status = PAM_AUTH_ERR;
    struct stat identity;
    if (!fstat(fd, &identity) && S_ISREG(identity.st_mode) &&
        identity.st_uid == 0 && (identity.st_mode & 0777) == 0600 &&
        !flock(fd, LOCK_EX | LOCK_NB)) {
        int initial = initial_password();
        int changed = initial == 1 && set_password(login) == PAM_SUCCESS;
        if ((initial == 0 || (changed && initial_password() == 0)) &&
            (!unlink(pending) || errno == ENOENT))
            status = PAM_IGNORE;
    }
    close(fd);
    // The following common-auth stack still authenticates the new password.
    return status;
}

PAM_EXTERN int pam_sm_setcred(pam_handle_t *login, int flags,
                             int argc, const char **argv)
{
    (void)login;
    (void)flags;
    (void)argc;
    (void)argv;
    return PAM_IGNORE;
}
