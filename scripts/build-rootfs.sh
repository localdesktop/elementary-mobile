#!/usr/bin/env bash
# Build the elementary OS root filesystem that ships inside the APK.
#
#   scripts/build-rootfs.sh
#
# Writes assets/rootfs/elementary-os-rootfs.tar.xz and its manifest, elementary-os-rootfs.json,
# from rootfs/Dockerfile. Needs Docker with buildx. On an arm64 host (Apple silicon, arm64 Linux,
# GitHub's ubuntu-24.04-arm runners) everything runs natively; elsewhere Docker needs QEMU binfmt
# for linux/arm64 and is much slower.
#
# Environment:
#   ROOTFS_USERNAME   desktop user created in the image (default: elementary)
#   XZ_LEVEL          xz preset (default: -9e). -6 is much faster for local iterations.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out_dir="$root/assets/rootfs"

docker buildx build \
    --platform linux/arm64 \
    --target archive \
    --build-arg "USERNAME=${ROOTFS_USERNAME:-elementary}" \
    --build-arg "XZ_LEVEL=${XZ_LEVEL:--9e}" \
    --output "type=local,dest=$out_dir" \
    "$root/rootfs"

ls -lh "$out_dir"
cat "$out_dir/elementary-os-rootfs.json"
