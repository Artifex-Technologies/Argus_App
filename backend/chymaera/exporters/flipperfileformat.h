#pragma once

#include <QList>
#include <QPair>
#include <QString>
#include <QVariantMap>

namespace Chymaera {

/* Parser for the Flipper File Format (FFF) — the plain-text key/value format
 * the Flipper Zero uses for .nfc, .sub (Sub-GHz), .rfid, .ir and similar dump
 * files. Feeds the NFC/RFID workbench and RF analysis views (Roadmap Phase 3).
 *
 * FFF grammar (as produced by Flipper firmware):
 *   Filetype: <string>
 *   Version: <int>
 *   # comment lines start with a hash
 *   Key: value
 * Some keys legitimately repeat (e.g. RAW_Data in Sub-GHz files), so the parser
 * preserves order and duplicates. */

class FlipperFileFormat
{
public:
    FlipperFileFormat();

    bool loadFromFile(const QString &path);
    bool loadFromData(const QByteArray &data);

    QString filetype() const;
    int version() const;

    bool contains(const QString &key) const;
    QString value(const QString &key, const QString &fallback = QString()) const;
    QStringList values(const QString &key) const; // all values for a repeated key

    // Ordered list of all (key, value) pairs as parsed.
    const QList<QPair<QString, QString>> &pairs() const;

    QString lastError() const;

private:
    QList<QPair<QString, QString>> m_pairs;
    QString m_filetype;
    int m_version;
    QString m_lastError;
};

/* Typed convenience parsers. Each returns a QVariantMap summarising the file
 * (empty on failure) so the bridge/control API and CLI can present or store it
 * without knowing FFF internals. A "kind" key labels the result. */
namespace FlipperFiles {

// .nfc — extracts device type, UID, ATQA/SAK and any protocol-specific fields.
QVariantMap parseNfc(const QString &path);

// .sub — extracts frequency, preset, protocol and RAW timing data (if present).
QVariantMap parseSubGhz(const QString &path);

// Dispatches on file extension to the right parser above; returns a generic
// key/value dump for unknown FFF files.
QVariantMap parseAuto(const QString &path);

}

}
