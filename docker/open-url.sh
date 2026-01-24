#!/usr/bin/env bash
# Browser wrapper for containerized Claude Code
# Prints OAuth URLs so users can manually open them on the host

set -euo pipefail

url="${1:-}"

if [[ -z "$url" ]]; then
    echo "Usage: open-url <URL>" >&2
    exit 1
fi

# Print URL prominently for manual opening
echo ""
echo "=============================================="
echo "Please open this URL in your browser:"
echo ""
echo "  $url"
echo ""
echo "=============================================="
echo ""
