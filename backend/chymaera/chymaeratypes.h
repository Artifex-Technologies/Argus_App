#pragma once

#include <QString>

/* Shared vocabulary for the Chymaera subsystem.
 *
 * Kept deliberately small and free of QObject/moc namespace machinery so it can
 * be included from every Chymaera translation unit (datastore, event log,
 * exporters, control server) without ordering headaches. Enums are plain; the
 * event log exposes them to QML as an int role plus a human-readable string
 * role, which is all the UI needs. */

namespace Chymaera {

// How important a log entry is. Ordering matters: higher == more severe.
enum class Severity {
    Debug = 0,
    Info,
    Notice,
    Warning,
    Error,
    Critical
};

// Where a log entry originated.
enum class Source {
    System = 0, // qFlipper/Chymaera plumbing itself
    Device,     // the connected Flipper Zero
    Sarina,     // the Sarina control API (assistant layer)
    Capture,    // imported/streamed capture data
    Export,     // exporters (pcap, etc.)
    User        // manual operator input
};

QString severityName(Severity s);
QString sourceName(Source s);

// Parse a name back to an enum (case-insensitive). Falls back to the given
// default when the text is not recognised.
Severity severityFromName(const QString &name, Severity fallback = Severity::Info);
Source sourceFromName(const QString &name, Source fallback = Source::User);

}
