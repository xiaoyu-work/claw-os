#!/usr/bin/env bash

package_version() {
    local project_dir="${1:?project root required}"
    local version="${COS_PACKAGE_VERSION:-}"

    if [ -z "$version" ]; then
        local base count sha
        base="$(sed -n 's/^version = "\(.*\)"/\1/p' \
            "$project_dir/core/Cargo.toml" | head -1)"
        if [ -z "$base" ]; then
            echo "error: cannot read base version from core/Cargo.toml" >&2
            return 1
        fi
        if ! git -C "$project_dir" rev-parse --git-dir >/dev/null 2>&1; then
            echo "error: package version requires a git checkout" >&2
            return 1
        fi
        if [ "$(git -C "$project_dir" rev-parse --is-shallow-repository)" = "true" ]; then
            echo "error: shallow checkout cannot produce a monotonic package version" >&2
            echo "       fetch full history or set COS_PACKAGE_VERSION explicitly" >&2
            return 1
        fi
        if [ -n "$(git -C "$project_dir" status --porcelain --untracked-files=normal)" ]; then
            echo "error: dirty worktree cannot reuse a commit-derived package version" >&2
            echo "       commit changes or set COS_PACKAGE_VERSION explicitly" >&2
            return 1
        fi
        count="$(git -C "$project_dir" rev-list --count HEAD)"
        sha="$(git -C "$project_dir" rev-parse --short=12 HEAD)"
        if [ "${GITHUB_EVENT_NAME:-}" = "pull_request" ]; then
            local pr="${GITHUB_REF:-pr}"
            pr="${pr#refs/pull/}"
            pr="${pr%%/*}"
            [[ "$pr" =~ ^[0-9]+$ ]] || pr=0
            version="${base}~pr${pr}.git${count}.g${sha}"
        else
            version="${base}+git${count}.g${sha}"
        fi
    fi

    if ! command -v dpkg >/dev/null 2>&1 \
        || ! dpkg --validate-version "$version" >/dev/null 2>&1; then
        echo "error: invalid Debian package version: $version" >&2
        return 1
    fi
    printf '%s\n' "$version"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    PROJECT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
    package_version "$PROJECT_DIR"
fi
