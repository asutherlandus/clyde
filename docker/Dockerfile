# Clyde - Docker Container for Claude Code
# Version: 1.0.0

# Base image: Ubuntu 24.04 for glibc compatibility with npm native modules
FROM ubuntu:24.04

# Build arguments
ARG NODE_VERSION=20

# Suppress apt prompts during build
ENV DEBIAN_FRONTEND=noninteractive

# Install system packages
# Layer 1: Base system packages
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    ca-certificates \
    git \
    openssh-client \
    tini \
    gosu \
    xdg-utils \
    && rm -rf /var/lib/apt/lists/*

# Layer 2: Install Node.js via NodeSource
RUN curl -fsSL https://deb.nodesource.com/setup_${NODE_VERSION}.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# Layer 3: Install Claude Code globally via npm
RUN npm install -g @anthropic-ai/claude-code

# Layer 4: Copy entrypoint script
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# Working directory (will be overridden by volume mount)
WORKDIR /workspace

# Use tini as init process for proper signal handling
ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]

# Default command: launch Claude Code with skip-permissions
# Container isolation provides the security boundary
CMD ["claude", "--dangerously-skip-permissions"]
