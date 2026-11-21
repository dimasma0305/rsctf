#!/bin/sh
# Package trusted Alpine linux-virt and BuildKit binaries into a bootable guest.
set -eu
test "$#" -eq 2 || { echo "usage: sh make-initramfs.sh BUILDKIT_BIN_DIR OUTPUT_DIR" >&2; exit 2; }
tools=$(realpath "$1")
mkdir -p "$2"
out=$(realpath "$2")
init=$(dirname "$(realpath "$0")")/init
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
mkdir "$root/files"
cd "$root/files"
mkdir bin lib dev proc sys tmp work etc run
install -m 755 "$init" init
install -m 755 /bin/busybox "$tools/buildctl" "$tools/buildkitd" "$tools/buildkit-runc" bin/
ln -s busybox bin/sh
install -m 755 /lib/ld-musl-x86_64.so.1 lib/
gzip -dc /lib/modules/*-virt/kernel/drivers/block/virtio_blk.ko.gz > virtio_blk.ko
# cpio records UID 0 even when packaging as an unprivileged user.
find . -print | LC_ALL=C sort > "$root/list"
cpio --quiet -o -H newc --owner=0:0 < "$root/list" > "$out/initramfs.cpio"
install -m 644 /boot/vmlinuz-virt "$out/vmlinuz"
