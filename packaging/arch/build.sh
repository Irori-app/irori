#!/usr/bin/env bash
# Build Irori's Arch packages from the Linux tarball and install them the way
# users will: from a pacman repository. Runs as root in an archlinux container.
#
#   packaging/arch/build.sh <version> <dist dir with the tarball> <output dir> <owner/repo>
set -euo pipefail

version=$1
dist=$(realpath "$2")
mkdir -p "$3"
out=$(realpath "$3")
repo=$4
root=$(cd "$(dirname "$0")/../.." && pwd)

# pacman's download sandbox (seccomp, Landlock, a separate download user) does
# not work in every container.
sed -i -e 's/^#\?DisableSandboxSyscalls/DisableSandboxSyscalls/' \
  -e 's/^#\?DisableSandboxFilesystem/DisableSandboxFilesystem/' \
  -e 's/^DownloadUser/#DownloadUser/' /etc/pacman.conf
pacman -Syu --noconfirm --needed base-devel python

work=$(mktemp -d)
python3 "$root/packaging/release_manifests.py" --version "$version" --dist "$dist" \
  --repo "$repo" --output "$work" --arch-repository
cp "$dist/Irori-$version-linux.tar.gz" "$work/arch/"

# makepkg refuses to run as root.
id builder >/dev/null 2>&1 || useradd --create-home builder
chown -R builder "$work"
su builder -c "cd '$work/arch' && makepkg --nodeps --noconfirm"
cp "$work"/arch/*.pkg.tar.zst "$out/"

# A local repository like packages.irori.app/arch, without signatures.
mkdir "$work/repo"
cp "$out"/*.pkg.tar.zst "$work/repo/"
repo-add "$work/repo/irori.db.tar.gz" "$work"/repo/*.pkg.tar.zst
cat >> /etc/pacman.conf <<EOF

[irori]
SigLevel = Optional TrustAll
Server = file://$work/repo
EOF

# Irori alone first: it must start without the sign-in window's libraries.
pacman -Sy --noconfirm irori
irori --version
pacman -S --noconfirm irori-sign-in
set +e
irori-sign-in --unsupported 2> "$work/error.txt"
status=$?
set -e
cat "$work/error.txt"
[ "$status" -eq 1 ] && grep -q 'unknown argument --unsupported' "$work/error.txt"
ls -l "$out"
