NAME = qFlipper

equals(QT_MAJOR_VERSION, 6): QT += core5compat

unix:!macx {
    DEFINES += USB_BACKEND_LIBUSB
    CONFIG += link_pkgconfig
    PKGCONFIG += libusb-1.0 zlib

    isEmpty(PREFIX): PREFIX = /usr

} else:win32 {
    CONFIG -= debug_and_release
    DEFINES += USB_BACKEND_WIN32
    INCLUDEPATH += $$[QT_INSTALL_HEADERS]/QtZlib

    !win32-g++: LIBS +=  -lSetupApi -lWinusb -lUser32
    else: LIBS += -lsetupapi -lwinusb

} else:macx {
    DEFINES += USB_BACKEND_LIBUSB
    PKG_CONFIG = /opt/homebrew/bin/pkg-config
    CONFIG += link_pkgconfig
    PKGCONFIG += libusb-1.0 zlib

} else {
    error("Unsupported OS or compiler")
}

# The app's version is its own: the VERSION file at the top of this repository (Labyrinth_OS's docs/VERSIONING.md; its
# scripts/release.sh writes the file and tags the release vX.Y.Z). APP_COMMIT tells builds between releases apart. The
# firmware keeps its own ARG-<upstream>r<N> names (Argus_Firmware); nothing here names it.
ARGUS_VERSION = $$cat($$PWD/VERSION, lines)
isEmpty(ARGUS_VERSION): error("no version in $$PWD/VERSION")

# The upstream qFlipper release this fork is based on: update it by hand when merging upstream. Not part of the version.
UPSTREAM_VERSION = 1.3.3

GIT_COMMIT = $$system("git rev-parse --short=8 HEAD","lines", HAS_COMMIT)
!equals(HAS_COMMIT, 0) {
    GIT_COMMIT = unknown
}

GIT_TIMESTAMP = $$system("git log -1 --pretty=format:%ct","lines", HAS_TIMESTAMP)
!equals(HAS_TIMESTAMP, 0) {
    GIT_TIMESTAMP = 0
}

# Argus is installed from source or by pacman: it never updates itself from upstream qFlipper's feed, whose AppImage it would
# download and run (2026-10-27 audit)
DEFINES += DISABLE_APPLICATION_UPDATES

DEFINES += APP_NAME=\\\"$$NAME\\\" \
           APP_VERSION=\\\"$$ARGUS_VERSION\\\" \
           APP_COMMIT=\\\"$$GIT_COMMIT\\\" \
           APP_TIMESTAMP=$$GIT_TIMESTAMP \
           PB_ENABLE_MALLOC

