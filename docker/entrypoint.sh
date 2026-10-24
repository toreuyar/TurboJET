#!/bin/bash
set -e

TARGET_UID="${HOST_UID:-1000}"
TARGET_GID="${HOST_GID:-1000}"

if [ "$(id -u)" = "0" ]; then
    if ! getent group opencode >/dev/null 2>&1; then
        groupadd -g "$TARGET_GID" opencode
    else
        groupmod -g "$TARGET_GID" opencode 2>/dev/null || true
    fi

    if ! id -u opencode >/dev/null 2>&1; then
        useradd -m -u "$TARGET_UID" -g "$TARGET_GID" -s /bin/bash opencode
    else
        usermod -u "$TARGET_UID" -g "$TARGET_GID" opencode 2>/dev/null || true
    fi

    chown -R opencode:opencode /home/opencode

    exec gosu opencode "$@"
else
    exec "$@"
fi
