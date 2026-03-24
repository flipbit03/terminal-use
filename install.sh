#!/bin/sh
# Install tu (terminal-use) — headless virtual terminal for AI agents
# Usage: curl -fsSL https://raw.githubusercontent.com/flipbit03/terminal-use/main/install.sh | sh
set -e

REPO="flipbit03/terminal-use"
INSTALL_DIR="${TU_INSTALL_DIR:-$HOME/.local/bin}"

# Detect OS and architecture.
OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}" in
  Linux)  OS_TAG="linux" ;;
  Darwin) OS_TAG="macos" ;;
  *) echo "Unsupported OS: ${OS}" >&2; exit 1 ;;
esac

case "${ARCH}" in
  x86_64|amd64)  ARCH_TAG="x86_64" ;;
  aarch64|arm64) ARCH_TAG="aarch64" ;;
  *) echo "Unsupported architecture: ${ARCH}" >&2; exit 1 ;;
esac

# macOS x86_64 binaries are not provided.
if [ "${OS}" = "Darwin" ] && [ "${ARCH_TAG}" = "x86_64" ]; then
  echo "macOS x86_64 binaries are not provided. Use: cargo install terminal-use" >&2
  exit 1
fi

ASSET="tu_${OS_TAG}_${ARCH_TAG}"

# Get latest release tag.
echo "Fetching latest release..."
TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | cut -d'"' -f4)
if [ -z "${TAG}" ]; then
  echo "Failed to determine latest release" >&2
  exit 1
fi
echo "Latest release: ${TAG}"

# Download binary.
URL="https://github.com/${REPO}/releases/download/${TAG}/${ASSET}"
echo "Downloading ${ASSET}..."
TMPDIR=$(mktemp -d)
trap 'rm -rf "${TMPDIR}"' EXIT

curl -fsSL "${URL}" -o "${TMPDIR}/tu"

# Install.
mkdir -p "${INSTALL_DIR}"
mv "${TMPDIR}/tu" "${INSTALL_DIR}/tu"
chmod +x "${INSTALL_DIR}/tu"

echo "Installed tu ${TAG} to ${INSTALL_DIR}/tu"

# Check if install dir is in PATH.
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *) echo "Add ${INSTALL_DIR} to your PATH:"; echo "  export PATH=\"${INSTALL_DIR}:\$PATH\"" ;;
esac
