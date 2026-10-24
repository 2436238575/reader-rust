#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

DOCKER_REPO="${DOCKER_REPO:-docker.io/givenge/reader-rust}"
BUMP_MODE="patch"
INPUT_VERSION=""
DRY_RUN=0

usage() {
  cat <<'USAGE'
Usage:
  ./scripts/release.sh [vX.Y.Z|X.Y.Z|--major|--minor|--patch] [--dry-run]

Behavior:
  - If version is provided: release that exact version.
  - If version is omitted: auto-bump the workspace version
    (Cargo.toml / package.json / frontend/package.json — all three must
    already be aligned) with patch (+1); --major/--minor bump accordingly.
    Git tags are only checked for existence, never used as the baseline:
    a version reset (e.g. pre-beta 0.1.0) must not jump back to the old
    tag sequence.
  - --dry-run: resolve and print the target version, then exit. No commit,
    tag, push or build happens.
  - Default Docker repo: docker.io/givenge/reader-rust
    (override with DOCKER_REPO env)
USAGE
}

for arg in "$@"; do
  case "$arg" in
    --major) BUMP_MODE="major" ;;
    --minor) BUMP_MODE="minor" ;;
    --patch) BUMP_MODE="patch" ;;
    --dry-run) DRY_RUN=1 ;;
    -h|--help) usage; exit 0 ;;
    v*|[0-9]*.[0-9]*.[0-9]*) INPUT_VERSION="$arg" ;;
    *)
      echo "Unknown argument: $arg" >&2
      usage
      exit 1
      ;;
  esac
done

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

require_cmd git
require_cmd cargo
require_cmd npm
require_cmd node
require_cmd awk

# dry-run 只做只读的版本推导，不触碰工作区，也不需要 podman/gh
if [[ "$DRY_RUN" -eq 0 ]]; then
  require_cmd podman
  require_cmd gh

  if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "Working tree is not clean. Commit/stash changes first." >&2
    exit 1
  fi

  if [[ -n "$(git ls-files --others --exclude-standard)" ]]; then
    echo "Untracked files exist. Clean them first." >&2
    exit 1
  fi

  if ! gh auth status >/dev/null 2>&1; then
    echo "GitHub CLI is not authenticated. Run: gh auth login" >&2
    exit 1
  fi
fi

normalize_version() {
  local raw="$1"
  raw="${raw#v}"
  if [[ ! "$raw" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Invalid version: $1" >&2
    exit 1
  fi
  echo "$raw"
}

# 读 Cargo.toml [package] 段的 version；分段定位逻辑与 update_cargo_version 保持一致
read_cargo_version() {
  awk '
    /^\[package\]/ { in_pkg=1; next }
    /^\[/ { in_pkg=0 }
    in_pkg && /^version[[:space:]]*=/ {
      split($0, parts, "\"")
      print parts[2]
      exit
    }
  ' Cargo.toml
}

# 版本基线取 workspace 三处版本号（发布脚本每次都会同步写它们），
# 不取 git tag：仓库版本可能整体重置（如 pre-beta 回到 0.1.0）而历史
# tag 停在更高的旧序列上，按 tag 递增会把下一个版本号跳回旧序列。
cargo_version="$(read_cargo_version)"
root_version="$(node -p "require('./package.json').version")"
frontend_version="$(node -p "require('./frontend/package.json').version")"

if [[ -z "$cargo_version" || -z "$root_version" || -z "$frontend_version" ]]; then
  echo "Failed to read workspace versions: Cargo.toml=${cargo_version:-?}, package.json=${root_version:-?}, frontend/package.json=${frontend_version:-?}" >&2
  exit 1
fi

if [[ "$cargo_version" != "$frontend_version" || "$root_version" != "$frontend_version" ]]; then
  echo "Workspace versions are inconsistent:" >&2
  echo "  Cargo.toml            = ${cargo_version}" >&2
  echo "  package.json          = ${root_version}" >&2
  echo "  frontend/package.json = ${frontend_version}" >&2
  echo "Align them first; every release writes all three in lockstep." >&2
  exit 1
fi

if [[ -n "$INPUT_VERSION" ]]; then
  SEMVER="$(normalize_version "$INPUT_VERSION")"
else
  base="$(normalize_version "$frontend_version")"
  IFS='.' read -r major minor patch <<<"$base"
  case "$BUMP_MODE" in
    major)
      SEMVER="$((major + 1)).0.0"
      ;;
    minor)
      SEMVER="${major}.$((minor + 1)).0"
      ;;
    patch)
      SEMVER="${major}.${minor}.$((patch + 1))"
      ;;
    *)
      echo "Invalid bump mode: $BUMP_MODE" >&2
      exit 1
      ;;
  esac
fi

TAG="v${SEMVER}"
echo "Releasing ${TAG}"

# git tag 只用于存在性校验：目标 tag 已存在说明该版本发布过
if git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
  echo "Tag ${TAG} already exists locally." >&2
  exit 1
fi

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "Dry run: target ${TAG} (workspace base ${frontend_version}, mode ${BUMP_MODE}${INPUT_VERSION:+, explicit ${INPUT_VERSION}})."
  echo "Nothing was committed, tagged, built or pushed."
  exit 0
fi

if gh release view "$TAG" >/dev/null 2>&1; then
  echo "GitHub release ${TAG} already exists." >&2
  exit 1
fi

update_cargo_version() {
  local version="$1"
  local file="Cargo.toml"
  local tmp
  tmp="$(mktemp)"
  awk -v ver="$version" '
    BEGIN { in_pkg=0; done=0 }
    /^\[package\]/ { in_pkg=1; print; next }
    /^\[/ && $0 !~ /^\[package\]/ { in_pkg=0 }
    in_pkg && !done && /^version[[:space:]]*=/ {
      print "version = \"" ver "\""
      done=1
      next
    }
    { print }
  ' "$file" > "$tmp"
  mv "$tmp" "$file"
}

update_cargo_version "$SEMVER"

# 只把 workspace 成员的新版本号推进 Cargo.lock（-w 不动依赖树），
# 再用 --locked 自检一致性：Dockerfile 里 cargo build --release --locked
# 在 lock 落后于 manifest 时会直接构建失败，必须在这里提前拦下
cargo update -w
if ! cargo metadata --format-version 1 --locked >/dev/null; then
  echo "Cargo.lock is still out of sync with Cargo.toml after 'cargo update -w'." >&2
  exit 1
fi

npm version "$SEMVER" --no-git-tag-version --allow-same-version >/dev/null
(
  cd frontend
  npm version "$SEMVER" --no-git-tag-version --allow-same-version >/dev/null
)

# 前端与后端都在 Docker 多阶段构建里从源码编译（可复现）；
# 本机不再预编译（编译正确性由 CI 的 cargo test + npm run build 兜底）。
echo "Creating commit and tag..."
git add Cargo.toml Cargo.lock package.json package-lock.json frontend/package.json frontend/package-lock.json
if git diff --cached --quiet; then
  echo "Version files already match ${TAG}; tagging current commit."
else
  git commit -m "release: ${TAG}"
fi
git tag -a "$TAG" -m "$TAG"

echo "Pushing git refs..."
current_branch="$(git branch --show-current)"
git push origin "$current_branch"
git push origin "$TAG"

echo "Building Docker images..."
podman build --platform linux/amd64 --build-arg TARGET_TRIPLE=x86_64-unknown-linux-musl -t "${DOCKER_REPO}:${TAG}-x86_64" -f Dockerfile .
podman build --platform linux/arm64 --build-arg TARGET_TRIPLE=aarch64-unknown-linux-musl -t "${DOCKER_REPO}:${TAG}-aarch64" -f Dockerfile .

echo "Verifying image architectures..."
arch_x86="$(podman image inspect "${DOCKER_REPO}:${TAG}-x86_64" --format '{{.Architecture}} {{.Os}}')"
arch_arm="$(podman image inspect "${DOCKER_REPO}:${TAG}-aarch64" --format '{{.Architecture}} {{.Os}}')"
echo "${TAG}-x86_64 => ${arch_x86}"
echo "${TAG}-aarch64 => ${arch_arm}"

if [[ "$arch_x86" != "amd64 linux" ]]; then
  echo "Unexpected x86 image architecture: $arch_x86" >&2
  exit 1
fi
if [[ "$arch_arm" != "arm64 linux" ]]; then
  echo "Unexpected arm image architecture: $arch_arm" >&2
  exit 1
fi

echo "Pushing versioned Docker tags..."
podman push "${DOCKER_REPO}:${TAG}-x86_64"
podman push "${DOCKER_REPO}:${TAG}-aarch64"

echo "Updating rolling Docker tags..."
podman tag "${DOCKER_REPO}:${TAG}-x86_64" "${DOCKER_REPO}:latest"
podman tag "${DOCKER_REPO}:${TAG}-aarch64" "${DOCKER_REPO}:latest-aarch64"
podman push "${DOCKER_REPO}:latest"
podman push "${DOCKER_REPO}:latest-aarch64"

echo "Creating GitHub release..."
gh release create "$TAG" \
  --title "$TAG" \
  --generate-notes

echo "Release completed: $TAG"
echo "GitHub release: https://github.com/givenge/reader-rust/releases/tag/${TAG}"
echo "Docker tags:"
echo "  ${DOCKER_REPO}:${TAG}-x86_64"
echo "  ${DOCKER_REPO}:${TAG}-aarch64"
echo "  ${DOCKER_REPO}:latest"
echo "  ${DOCKER_REPO}:latest-aarch64"
