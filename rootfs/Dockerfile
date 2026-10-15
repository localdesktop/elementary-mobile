# syntax=docker/dockerfile:1
#
# The elementary OS root filesystem that ships inside the APK: Ubuntu 24.04 (arm64) plus the
# elementary OS 8 packages from ppa:elementary-os/stable and ppa:elementary-os/os-patches.
# elementary does not publish an arm64 image to start from, so it is assembled from the same
# packages elementary OS is made of. Build it with scripts/build-rootfs.sh, which exports the
# `archive` stage (the compressed tarball and its manifest) into assets/rootfs/.

ARG BASE=ubuntu:24.04

# Ubuntu's Xwayland with the patches in patches/ applied.
FROM ${BASE} AS xwayland
ENV DEBIAN_FRONTEND=noninteractive \
    DEBEMAIL=noreply@elementary-mobile.invalid \
    DEBFULLNAME="elementary-mobile"
RUN sed -i 's/^Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/ubuntu.sources \
 && apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates devscripts dpkg-dev \
 && apt-get build-dep -y xwayland
COPY patches/ /patches/
WORKDIR /build
RUN apt-get source xwayland \
 && cd xwayland-*/ \
 && for patch in /patches/xwayland-*.patch; do \
        cp "$patch" debian/patches/ && basename "$patch" >> debian/patches/series; \
    done \
 && dch --local +elementarymobile "Rootful: advertise the current size as the preferred RandR mode." \
 && DEB_BUILD_OPTIONS="nocheck parallel=$(nproc)" dpkg-buildpackage -b -uc -us \
 && mkdir /debs \
 && cp ../xwayland_*.deb /debs/

FROM ${BASE} AS rootfs
ARG USERNAME=elementary
ENV DEBIAN_FRONTEND=noninteractive

# Path filters and apt defaults must be in place before anything is installed.
COPY overlay/etc/dpkg/ /etc/dpkg/
COPY overlay/etc/apt/ /etc/apt/
COPY keys/elementary-os-ppa.asc /etc/apt/keyrings/elementary-os-ppa.asc
COPY packages.list /tmp/packages.list
COPY --from=xwayland /debs/ /tmp/debs/

RUN apt-get update \
 && apt-get install -y ca-certificates \
 && for ppa in stable os-patches; do \
        printf 'Types: deb\nURIs: https://ppa.launchpadcontent.net/elementary-os/%s/ubuntu\nSuites: noble\nComponents: main\nSigned-By: /etc/apt/keyrings/elementary-os-ppa.asc\n' "$ppa" \
            > "/etc/apt/sources.list.d/elementary-os-$ppa.sources"; \
    done \
 && apt-get update \
 && apt-get install -y elementary-os-overlay \
 && apt-get dist-upgrade -y \
 # Ubuntu's epiphany-browser depends on Ubuntu's libgranite-7-7, which conflicts with elementary's
 # newer Granite 7. Both ship the same soname (libgranite-7.so.7), so an empty package that pulls
 # in elementary's libgranite7 satisfies it.
 && mkdir -p /tmp/granite-shim/DEBIAN \
 && printf 'Package: libgranite-7-7\nVersion: 7.8.1+elementarymobile1\nArchitecture: %s\nMaintainer: elementary-mobile <noreply@elementary-mobile.invalid>\nDepends: libgranite7 (>= 7.8.1)\nSection: libs\nPriority: optional\nDescription: libgranite-7-7 ABI provided by elementary libgranite7\n' \
        "$(dpkg --print-architecture)" > /tmp/granite-shim/DEBIAN/control \
 && dpkg-deb --build /tmp/granite-shim /tmp/debs/libgranite-7-7-shim.deb \
 && apt-get install -y $(grep -v -e '^#' -e '^$' /tmp/packages.list) /tmp/debs/*.deb \
 && apt-mark hold xwayland libgranite-7-7 \
 && apt-get clean

RUN userdel -r ubuntu 2>/dev/null; \
    useradd --create-home --uid 1000 --shell /bin/bash --groups sudo "$USERNAME" \
 && passwd --delete "$USERNAME" \
 && printf '%s ALL=(ALL) NOPASSWD:ALL\n' "$USERNAME" > /etc/sudoers.d/elementary-mobile \
 && chmod 0440 /etc/sudoers.d/elementary-mobile \
 && locale-gen en_US.UTF-8

COPY overlay/ /

RUN glib-compile-schemas /usr/share/glib-2.0/schemas \
 # Generated per device by the app.
 && rm -f /etc/machine-id /var/lib/dbus/machine-id \
 && rm -rf /tmp/* /var/tmp/* /var/lib/apt/lists/* /var/cache/apt/*.bin /var/cache/debconf/*-old \
           /var/lib/dpkg/*-old /root/.cache \
 && find /var/log -type f -delete

# Pack the root filesystem inside a container rather than with `--output type=tar`: some Docker
# hosts add files of their own to that export (OrbStack adds macOS volume icons), and this way
# the archive and its manifest come out the same everywhere.
FROM ${BASE} AS packer
ARG USERNAME=elementary
ARG XZ_LEVEL=-9e
RUN apt-get update \
 && apt-get install -y --no-install-recommends xz-utils \
 && rm -rf /var/lib/apt/lists/*
COPY --from=rootfs / /rootfs/
# xz beats zstd by ~12% here and still decompresses 1 GiB in a few seconds on a phone, where
# creating the files dominates. Multithreaded xz also splits the stream into independent blocks.
RUN mkdir /out \
 && tar --create --sort=name --numeric-owner --format=gnu --directory=/rootfs --file=/tmp/rootfs.tar . \
 && uncompressed_size=$(stat -c %s /tmp/rootfs.tar) \
 && entries=$(tar --list --file=/tmp/rootfs.tar | wc -l) \
 && xz --threads=0 --memlimit-compress=50% "$XZ_LEVEL" --stdout /tmp/rootfs.tar > /out/elementary-os-rootfs.tar.xz \
 && rm /tmp/rootfs.tar \
 && printf '{\n  "id": "%s",\n  "archive": "elementary-os-rootfs.tar.xz",\n  "compressed_size": %s,\n  "uncompressed_size": %s,\n  "entries": %s,\n  "username": "%s",\n  "built_at": "%s"\n}\n' \
        "$(sha256sum /out/elementary-os-rootfs.tar.xz | cut -d' ' -f1)" \
        "$(stat -c %s /out/elementary-os-rootfs.tar.xz)" "$uncompressed_size" "$entries" \
        "$USERNAME" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        > /out/elementary-os-rootfs.json

FROM scratch AS archive
COPY --from=packer /out/ /
