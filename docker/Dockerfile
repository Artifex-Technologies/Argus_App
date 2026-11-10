# Argus_App build container: Arch Linux with Qt 6 from the distro, the docs/LINUX_BUILD.md route.
# Upstream's Ubuntu 20.04 image pulled static Qt from an openSUSE OBS repo whose key now returns
# 404, so it no longer builds. This one builds the same way as .github/workflows/linux.yml:
#   docker compose up -d        (or: podman compose up -d)
#   docker compose exec dev .github/scripts/package_linux.sh
FROM archlinux:base-devel

RUN pacman -Syu --noconfirm --needed \
        git \
        qt6-base \
        qt6-declarative \
        qt6-svg \
        qt6-5compat \
        qt6-serialport \
        qt6-tools \
        libusb \
        sqlite \
        zlib \
    && rm -rf /var/cache/pacman/pkg/* \
    && git config --system --add safe.directory /project

WORKDIR /project
