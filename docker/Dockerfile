# Clyde - Docker Container for Claude Code
# Version: 1.1.0

# Base image: Ubuntu 24.04 for glibc compatibility with npm native modules
# Pinned to specific digest for reproducibility
FROM ubuntu:24.04@sha256:cd1dba651b3080c3686ecf4e3c4220f026b521fb76978881737d24f200828b2b

# Build arguments
ARG NODE_VERSION=20
# Pin claude-code version for reproducible builds
ARG CLAUDE_CODE_VERSION=2.1.19

# Suppress apt prompts during build
ENV DEBIAN_FRONTEND=noninteractive

# Set browser wrapper for OAuth URL printing
ENV BROWSER=/usr/local/bin/open-url

# Install system packages with pinned versions
# Layer 1: Base system packages
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl=8.5.0-2ubuntu10.6 \
    ca-certificates=20240203 \
    git=1:2.43.0-1ubuntu7.3 \
    openssh-client=1:9.6p1-3ubuntu13.14 \
    tini=0.19.0-1 \
    gosu=1.17-1ubuntu0.24.04.3 \
    xdg-utils=1.1.3-4.1ubuntu3 \
    gnupg=2.4.4-2ubuntu17.4 \
    && rm -rf /var/lib/apt/lists/*

# Layer 2: Install Node.js via NodeSource (download, verify, then execute)
# Uses GPG key verification instead of curl|bash pattern
RUN curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key \
        | gpg --dearmor -o /usr/share/keyrings/nodesource.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/nodesource.gpg] https://deb.nodesource.com/node_${NODE_VERSION}.x nodistro main" \
        > /etc/apt/sources.list.d/nodesource.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# Layer 3: Install Claude Code globally via npm with pinned version
RUN npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}

# Layer 4: Copy entrypoint and browser wrapper scripts
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
