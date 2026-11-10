# Argus features on top of qFlipper: app extraction and check-up, Limbo rebuilds, SD snapshots,
# the capture library. The logic is the Rust crate in argus-tools/ (run through QProcess); this
# is the device side and the controller the QML and the Sarina API use. Included from
# chymaera/chymaera.pri so no upstream project file changes.

SOURCES += \
    $$PWD/argustools.cpp \
    $$PWD/devicestorage.cpp \
    $$PWD/arguscontroller.cpp

HEADERS += \
    $$PWD/argustools.h \
    $$PWD/devicestorage.h \
    $$PWD/arguscontroller.h

# Build the argus-tools binary with cargo when it is installed (it lands in
# argus-tools/target/release, where a shadow build in <repo>/build finds it). Without cargo
# the app still builds; the Argus tools window then says the binary is missing.
argus_tools.target = argus_tools_bin
argus_tools.commands = @if command -v cargo >/dev/null 2>&1; then \
    cd $$shell_quote($$PWD/../../argus-tools) && nice -n 15 cargo build --release --quiet; \
    else echo argus-tools: cargo not found, skipping - the Argus tools window needs it; fi
argus_tools.depends = FORCE
QMAKE_EXTRA_TARGETS += argus_tools
unix: PRE_TARGETDEPS += argus_tools_bin
