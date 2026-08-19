#!/usr/bin/env bash
# Run Git queries without refreshing or rewriting the caller's index.
#
# `git status` normally refreshes cached stat data and atomically replaces
# .git/index. During a sudo build that replacement becomes root-owned and the
# repository owner can no longer run Git. --no-optional-locks keeps read-only
# queries read-only while preserving mandatory locks for commands that really
# modify a repository.

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    echo "error: scripts/lib/git-readonly.sh must be sourced, not executed" >&2
    exit 1
fi

git_readonly() {
    command git --no-optional-locks "$@"
}
