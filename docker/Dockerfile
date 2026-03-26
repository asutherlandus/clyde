# Clyde - Docker Container for Claude Code
# Version: 2.0.0
#
# This image uses a hybrid approach:
# - Nix provides project dependencies (git, gh, Node.js, etc.) with flake.lock pinning
# - Claude Code is installed via npm at runtime for always-latest version
# - Base image is minimal Ubuntu for glibc compatibility

# Base image: Ubuntu 24.04 minimal (~40MB vs ~78MB full)
# Pinned to specific digest for reproducibility
FROM ubuntu:24.04@sha256:cd1dba651b3080c3686ecf4e3c4220f026b521fb76978881737d24f200828b2b

# Suppress apt prompts during build
ENV DEBIAN_FRONTEND=noninteractive

# Set browser wrapper for OAuth URL printing
ENV BROWSER=/usr/local/bin/open-url

# Layer 1: Install minimal system packages
# - curl, ca-certificates, xz-utils: Required for Nix installation
# - tini: Init process for proper signal handling
# - gosu: Drop privileges to non-root user
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl=8.5.0-2ubuntu10.6 \
    ca-certificates=20240203 \
    xz-utils=5.6.1+really5.4.5-1ubuntu0.2 \
    tini=0.19.0-1 \
    gosu=1.17-1ubuntu0.24.04.3 \
    && rm -rf /var/lib/apt/lists/*

# Layer 2: Configure Nix settings before installation
# Disable build-users-group since we're running in single-user mode in Docker
RUN mkdir -p /etc/nix && \
    echo "experimental-features = nix-command flakes" > /etc/nix/nix.conf && \
    echo "build-users-group =" >> /etc/nix/nix.conf && \
    mkdir -p /root/.config/nix && \
    cp /etc/nix/nix.conf /root/.config/nix/nix.conf

# Layer 3: Install Nix manually in single-user mode
# The official installer doesn't work well as root in Docker, so we install manually
ARG NIX_VERSION=2.24.10
RUN mkdir -p /nix && \
    curl -L "https://releases.nixos.org/nix/nix-${NIX_VERSION}/nix-${NIX_VERSION}-x86_64-linux.tar.xz" | tar -xJ -C /tmp && \
    /tmp/nix-${NIX_VERSION}-x86_64-linux/install --no-daemon && \
    rm -rf /tmp/nix-${NIX_VERSION}-x86_64-linux

# Layer 4: Copy Nix configurations and generate flake.lock
COPY nix/ /docker/nix/
RUN chmod +x /docker/nix/clyde-packages /docker/nix/user-init.sh && \
    ln -s /docker/nix/clyde-packages /usr/local/bin/clyde-packages && \
    /root/.nix-profile/bin/nix flake lock /docker/nix

# Layer 5: Install agent-browser and Chrome for Testing
# agent-browser is a Rust-based headless browser CLI distributed via npm
# Node.js/npm lives inside the Nix devShell, so we use `nix develop` to access it
# We set npm prefix to /usr/local so the binary lands on the default PATH
# Chrome for Testing is downloaded via agent-browser install --with-deps
ARG AGENT_BROWSER_VERSION=0.22.3
RUN --mount=type=cache,target=/root/.npm \
    /root/.nix-profile/bin/nix develop /docker/nix --command bash -c "\
        npm install -g --prefix /usr/local agent-browser@${AGENT_BROWSER_VERSION} && \
        export PATH=/usr/local/bin:\$PATH && \
        agent-browser install" && \
    mv /root/.agent-browser/browsers /opt/browsers && \
    chmod -R o+rx /opt/browsers && \
    apt-get update && apt-get install -y --no-install-recommends \
        libglib2.0-0t64 libnspr4 libnss3 libatk1.0-0t64 libatk-bridge2.0-0t64 \
        libdbus-1-3 libcups2t64 libexpat1 libxcb1 libxkbcommon0 libasound2t64 \
        libgbm1 libx11-6 libxext6 libcairo2 libpango-1.0-0 libxcomposite1 \
        libxdamage1 libxfixes3 libxrandr2 libatspi2.0-0t64 fonts-liberation \
    && rm -rf /var/lib/apt/lists/*

# Layer 6: Copy browser configuration and skill definition
COPY browser/ /docker/browser/
COPY skills/ /docker/skills/
RUN chmod +x /docker/browser/setup-browser.sh /docker/browser/agent-browser-disabled.sh && \
    ln -sf /docker/browser/agent-browser-disabled.sh /usr/local/bin/agent-browser

# Layer 7: Copy entrypoint and browser wrapper scripts
COPY entrypoint.sh /entrypoint.sh
COPY open-url.sh /usr/local/bin/open-url
RUN chmod +x /entrypoint.sh /usr/local/bin/open-url

# Working directory (will be overridden by volume mount)
WORKDIR /workspace

# Use tini as init process for proper signal handling
ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]

# Default command: launch Claude Code with skip-permissions
# Container isolation provides the security boundary
CMD ["claude", "--dangerously-skip-permissions"]
