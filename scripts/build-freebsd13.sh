#!/usr/bin/env bash
# Build a TrueNAS CORE 13 / FreeBSD 13.1 amd64 binary using the persistent
# project-local cache. The cache itself is deliberately ignored by Git.
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cache_dir="${HD_MOVIES_FREEBSD13_BUILD_DIR:-$project_dir/.freebsd13-build}"
sysroot="${HD_MOVIES_FREEBSD_SYSROOT:-$cache_dir/sysroot}"
libc_config="${HD_MOVIES_FREEBSD_LIBC_CONFIG:-$cache_dir/freebsd13-libc.conf}"
linker="${HD_MOVIES_FREEBSD_LINKER:-$cache_dir/zig-freebsd13-linker}"

for required_path in "$sysroot" "$libc_config" "$linker"; do
  if [[ ! -e "$required_path" ]]; then
    printf 'FreeBSD 13 build cache is incomplete: missing %s\n' "$required_path" >&2
    printf 'Expected persistent cache: %s\n' "$cache_dir" >&2
    exit 1
  fi
done

if ! command -v "${ZIG:-zig}" >/dev/null 2>&1; then
  printf 'Zig compiler not found; set ZIG to its executable path.\n' >&2
  exit 1
fi

cd "$project_dir"
export HD_MOVIES_FREEBSD_SYSROOT="$sysroot"
export HD_MOVIES_FREEBSD_LIBC_CONFIG="$libc_config"
export CARGO_TARGET_X86_64_UNKNOWN_FREEBSD_LINKER="$linker"
export CC_x86_64_unknown_freebsd="$linker"
export AR_x86_64_unknown_freebsd=ar

exec cargo build --release --locked --target x86_64-unknown-freebsd "$@"
