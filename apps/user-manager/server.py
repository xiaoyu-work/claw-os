from claw_os_sdk.mcp import App

from main import (
    add_to_group,
    create_group,
    create_user,
    delete_group,
    delete_user,
    lock_user,
    remove_from_group,
    restore,
    set_password,
    set_shell,
    status,
    unlock_user,
)


app = App.from_manifest()


@app.tool("user-manager.status")
def user_manager_status() -> dict:
    return status()


@app.tool("user-manager.create-user")
def user_manager_create_user(
    user: str,
    groups: str | None = None,
    full_name: str | None = None,
    shell: str | None = None,
) -> dict:
    return create_user(
        user,
        groups=groups,
        full_name=full_name,
        shell=shell,
    )


@app.tool("user-manager.delete-user")
def user_manager_delete_user(user: str, confirm: bool) -> dict:
    return delete_user(user, confirm)


@app.tool("user-manager.lock-user")
def user_manager_lock_user(user: str) -> dict:
    return lock_user(user)


@app.tool("user-manager.unlock-user")
def user_manager_unlock_user(user: str) -> dict:
    return unlock_user(user)


@app.tool("user-manager.set-shell")
def user_manager_set_shell(user: str, shell: str) -> dict:
    return set_shell(user, shell)


@app.tool("user-manager.set-password")
def user_manager_set_password(user: str, credential: str) -> dict:
    return set_password(user, credential)


@app.tool("user-manager.create-group")
def user_manager_create_group(group: str) -> dict:
    return create_group(group)


@app.tool("user-manager.delete-group")
def user_manager_delete_group(group: str, confirm: bool) -> dict:
    return delete_group(group, confirm)


@app.tool("user-manager.add-to-group")
def user_manager_add_to_group(user: str, group: str) -> dict:
    return add_to_group(user, group)


@app.tool("user-manager.remove-from-group")
def user_manager_remove_from_group(user: str, group: str) -> dict:
    return remove_from_group(user, group)


@app.tool("user-manager.restore")
def user_manager_restore(backup_token: str, confirm: bool) -> dict:
    return restore(backup_token, confirm)


if __name__ == "__main__":
    app.serve()
