# Chymaera subsystem — operator-console extensions on top of qFlipper.
# Included from backend/backend.pro. Kept in its own .pri so the additions are
# isolated from upstream and easy to keep mergeable.

QT += sql network

# Make "chymaera/..." includes resolve from the backend/ root.
INCLUDEPATH += $$PWD/..

SOURCES += \
    $$PWD/chymaeratypes.cpp \
    $$PWD/chymaerabridge.cpp \
    $$PWD/datastore/chymaeradatastore.cpp \
    $$PWD/eventlog/chymaeraeventlog.cpp \
    $$PWD/exporters/pcapexporter.cpp \
    $$PWD/exporters/flipperfileformat.cpp \
    $$PWD/sarina/sarinaserver.cpp

HEADERS += \
    $$PWD/chymaeratypes.h \
    $$PWD/chymaerabridge.h \
    $$PWD/datastore/chymaeradatastore.h \
    $$PWD/eventlog/chymaeraeventlog.h \
    $$PWD/exporters/pcapexporter.h \
    $$PWD/exporters/flipperfileformat.h \
    $$PWD/sarina/sarinaserver.h
