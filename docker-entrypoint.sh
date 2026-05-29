#!/bin/sh
set -e

# Re-apply git/dolt config on every start so env var changes take effect
# even when the home volume already exists from a previous run.
if [ -n "$GIT_USER" ] && [ -n "$GIT_EMAIL" ]; then
    git config --global user.name "$GIT_USER"
    git config --global user.email "$GIT_EMAIL"
    git config --global credential.helper store
    dolt config --global --add user.name "$GIT_USER"
    dolt config --global --add user.email "$GIT_EMAIL"
fi

# Sync SSH keys from host bind-mount into agent home so git over SSH works.
# Runs every start so re-created agent-home volume gets keys back.
if [ -d /host-ssh ]; then
    mkdir -p /home/agent/.ssh
    chmod 700 /home/agent/.ssh
    for key in id_ed25519 id_ed25519.pub id_rsa id_rsa.pub known_hosts; do
        if [ -f "/host-ssh/$key" ] && [ ! -f "/home/agent/.ssh/$key" ]; then
            cp "/host-ssh/$key" "/home/agent/.ssh/$key"
        fi
    done
    chmod 600 /home/agent/.ssh/id_ed25519 2>/dev/null || true
    chmod 600 /home/agent/.ssh/id_rsa 2>/dev/null || true
    chmod 644 /home/agent/.ssh/*.pub /home/agent/.ssh/known_hosts 2>/dev/null || true
fi

if [ ! -f /gt/mayor/town.json ]; then
    echo "Initializing Gas Town workspace at /gt..."
    /app/gastown/gt install /gt --git
elif [ ! -d /gt/.beads ] || [ ! -f /gt/.beads/issues.jsonl ]; then
    echo "Refreshing Gas Town workspace at /gt..."
    /app/gastown/gt install /gt --git --force
else
    echo "Gas Town workspace at /gt already initialized — skipping install --force to preserve beads data."
fi

# Start the dashboard as a decoupled background child. It is NOT PID1's
# main process, so it can be stopped/restarted (e.g. to shed CPU) without
# killing the container — Dolt, the daemon, tmux and the town keep running.
# Set GT_DASHBOARD_AUTOSTART=0 to boot the container without the dashboard.
if [ "${GT_DASHBOARD_AUTOSTART:-1}" = "1" ]; then
    /app/gastown/gt dashboard \
        --bind "${GT_DASHBOARD_BIND:-0.0.0.0}" \
        --port "${GT_DASHBOARD_PORT:-8080}" &
    echo "dashboard started (pid $!) — decoupled from container lifecycle"
fi

# PID1's main is the keep-alive command ("$@", default 'sleep infinity' from
# the image CMD). tini reaps zombies. The container outlives any dashboard
# stop/restart, so killing the dashboard no longer tears down the town.
exec "$@"
