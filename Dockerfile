# Run with
# docker build -t gastown:latest -f Dockerfile .
FROM docker/sandbox-templates:claude-code

ARG GO_VERSION=1.25.8

USER root

# Install system dependencies
RUN apt-get update && apt-get install -y \
    build-essential \
    git \
    sqlite3 \
    tmux \
    curl \
    ripgrep \
    zsh \
    gh \
    netcat-openbsd \
    tini \
    vim \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/*

# Install Go from official tarball (apt golang-go is too old)
RUN ARCH=$(dpkg --print-architecture) && \
    curl -fsSL "https://go.dev/dl/go${GO_VERSION}.linux-${ARCH}.tar.gz" | tar -C /usr/local -xz
ENV PATH="/app/gastown:/usr/local/go/bin:/home/agent/go/bin:${PATH}"

# Install beads (bd) and dolt
RUN curl -fsSL https://raw.githubusercontent.com/steveyegge/beads/main/scripts/install.sh | bash
RUN curl -fsSL https://github.com/dolthub/dolt/releases/latest/download/install.sh | bash

# Install Rust (system-wide) for the gastown-rs workspace in apps/api/.
# CARGO_HOME world-writable so the unprivileged `agent` user can build (registry cache).
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:${PATH}
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
        sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable && \
    chmod -R a+w "$CARGO_HOME" "$RUSTUP_HOME"
# Make the toolchain visible to interactive login shells (bash + zsh / tmux sessions).
RUN printf 'export RUSTUP_HOME=/usr/local/rustup\nexport CARGO_HOME=/usr/local/cargo\nexport PATH=/usr/local/cargo/bin:$PATH\n' > /etc/profile.d/rust.sh && \
    printf 'export RUSTUP_HOME=/usr/local/rustup; export CARGO_HOME=/usr/local/cargo; export PATH=/usr/local/cargo/bin:$PATH\n' >> /etc/zsh/zshenv

# Set up directories
RUN mkdir -p /app /gt /gt/.dolt-data && chown -R agent:agent /app /gt

# Environment setup for bash and zsh
RUN echo 'export PATH="/app/gastown:$PATH"' >> /etc/profile.d/gastown.sh && \
    echo 'export PATH="/app/gastown:$PATH"' >> /etc/zsh/zshenv
RUN echo 'export COLORTERM="truecolor"' >> /etc/profile.d/colorterm.sh && \
    echo 'export COLORTERM="truecolor"' >> /etc/zsh/zshenv
RUN echo 'export TERM="xterm-256color"' >> /etc/profile.d/term.sh && \
    echo 'export TERM="xterm-256color"' >> /etc/zsh/zshenv

USER agent

COPY --chown=agent:agent . /app/gastown

RUN cd /app/gastown && make build

COPY --chown=agent:agent docker-entrypoint.sh /app/docker-entrypoint.sh
RUN chmod +x /app/docker-entrypoint.sh

WORKDIR /gt

ENTRYPOINT ["tini", "--", "/app/docker-entrypoint.sh"]
CMD ["sleep", "infinity"]
