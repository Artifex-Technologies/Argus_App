#include "flipperfileformat.h"

#include <QFile>
#include <QFileInfo>
#include <QRegularExpression>

using namespace Chymaera;

FlipperFileFormat::FlipperFileFormat():
    m_version(0)
{}

bool FlipperFileFormat::loadFromFile(const QString &path)
{
    QFile f(path);
    if(!f.open(QIODevice::ReadOnly | QIODevice::Text)) {
        m_lastError = QStringLiteral("Cannot open %1: %2").arg(path, f.errorString());
        return false;
    }
    return loadFromData(f.readAll());
}

bool FlipperFileFormat::loadFromData(const QByteArray &data)
{
    m_pairs.clear();
    m_filetype.clear();
    m_version = 0;
    m_lastError.clear();

    const QString text = QString::fromUtf8(data);
    const QStringList lines = text.split(QRegularExpression(QStringLiteral("\r\n|\n|\r")));

    for(const QString &rawLine : lines) {
        const QString line = rawLine.trimmed();
        if(line.isEmpty() || line.startsWith(QLatin1Char('#'))) {
            continue;
        }

        const int sep = line.indexOf(QLatin1Char(':'));
        if(sep < 0) {
            continue; // not a key: value line
        }

        const QString key = line.left(sep).trimmed();
        const QString value = line.mid(sep + 1).trimmed();
        if(key.isEmpty()) {
            continue;
        }

        m_pairs.append(qMakePair(key, value));

        if(key.compare(QStringLiteral("Filetype"), Qt::CaseInsensitive) == 0) {
            m_filetype = value;
        } else if(key.compare(QStringLiteral("Version"), Qt::CaseInsensitive) == 0) {
            m_version = value.toInt();
        }
    }

    if(m_pairs.isEmpty()) {
        m_lastError = QStringLiteral("No key/value pairs found (not a Flipper file?)");
        return false;
    }

    return true;
}

QString FlipperFileFormat::filetype() const
{
    return m_filetype;
}

int FlipperFileFormat::version() const
{
    return m_version;
}

bool FlipperFileFormat::contains(const QString &key) const
{
    for(const auto &p : m_pairs) {
        if(p.first.compare(key, Qt::CaseInsensitive) == 0) {
            return true;
        }
    }
    return false;
}

QString FlipperFileFormat::value(const QString &key, const QString &fallback) const
{
    for(const auto &p : m_pairs) {
        if(p.first.compare(key, Qt::CaseInsensitive) == 0) {
            return p.second;
        }
    }
    return fallback;
}

QStringList FlipperFileFormat::values(const QString &key) const
{
    QStringList out;
    for(const auto &p : m_pairs) {
        if(p.first.compare(key, Qt::CaseInsensitive) == 0) {
            out.append(p.second);
        }
    }
    return out;
}

const QList<QPair<QString, QString>> &FlipperFileFormat::pairs() const
{
    return m_pairs;
}

QString FlipperFileFormat::lastError() const
{
    return m_lastError;
}

// --- typed convenience parsers ---------------------------------------------

QVariantMap FlipperFiles::parseNfc(const QString &path)
{
    FlipperFileFormat fff;
    if(!fff.loadFromFile(path)) {
        return {};
    }

    QVariantMap out;
    out[QStringLiteral("kind")] = QStringLiteral("nfc");
    out[QStringLiteral("filetype")] = fff.filetype();
    out[QStringLiteral("version")] = fff.version();
    out[QStringLiteral("deviceType")] = fff.value(QStringLiteral("Device type"));
    out[QStringLiteral("uid")] = fff.value(QStringLiteral("UID"));
    out[QStringLiteral("atqa")] = fff.value(QStringLiteral("ATQA"));
    out[QStringLiteral("sak")] = fff.value(QStringLiteral("SAK"));

    // Surface any remaining protocol-specific fields verbatim.
    QVariantMap extra;
    static const QStringList known = {
        QStringLiteral("Filetype"), QStringLiteral("Version"), QStringLiteral("Device type"),
        QStringLiteral("UID"), QStringLiteral("ATQA"), QStringLiteral("SAK")
    };
    for(const auto &p : fff.pairs()) {
        if(!known.contains(p.first, Qt::CaseInsensitive)) {
            extra[p.first] = p.second;
        }
    }
    out[QStringLiteral("extra")] = extra;
    return out;
}

QVariantMap FlipperFiles::parseSubGhz(const QString &path)
{
    FlipperFileFormat fff;
    if(!fff.loadFromFile(path)) {
        return {};
    }

    QVariantMap out;
    out[QStringLiteral("kind")] = QStringLiteral("subghz");
    out[QStringLiteral("filetype")] = fff.filetype();
    out[QStringLiteral("version")] = fff.version();
    out[QStringLiteral("frequency")] = fff.value(QStringLiteral("Frequency"));
    out[QStringLiteral("preset")] = fff.value(QStringLiteral("Preset"));
    out[QStringLiteral("protocol")] = fff.value(QStringLiteral("Protocol"));

    // RAW_Data lines hold whitespace-separated signed durations (microseconds).
    QVector<qint32> raw;
    const QStringList rawChunks = fff.values(QStringLiteral("RAW_Data"));
    for(const QString &chunk : rawChunks) {
        const QStringList toks = chunk.split(QLatin1Char(' '), Qt::SkipEmptyParts);
        for(const QString &t : toks) {
            bool ok = false;
            const int v = t.toInt(&ok);
            if(ok) {
                raw.append(v);
            }
        }
    }
    out[QStringLiteral("rawSampleCount")] = raw.size();

    // Total on/off duration in microseconds — a cheap signal-length metric.
    qint64 totalUs = 0;
    for(qint32 v : raw) {
        totalUs += qAbs(static_cast<qint64>(v));
    }
    out[QStringLiteral("rawDurationUs")] = totalUs;
    return out;
}

QVariantMap FlipperFiles::parseAuto(const QString &path)
{
    const QString suffix = QFileInfo(path).suffix().toLower();

    if(suffix == QStringLiteral("nfc")) {
        return parseNfc(path);
    }
    if(suffix == QStringLiteral("sub")) {
        return parseSubGhz(path);
    }

    // Generic FFF dump for anything else (.rfid, .ir, .picopass, ...).
    FlipperFileFormat fff;
    if(!fff.loadFromFile(path)) {
        return {};
    }

    QVariantMap out;
    out[QStringLiteral("kind")] = QStringLiteral("generic");
    out[QStringLiteral("filetype")] = fff.filetype();
    out[QStringLiteral("version")] = fff.version();

    QVariantMap fields;
    for(const auto &p : fff.pairs()) {
        fields[p.first] = p.second;
    }
    out[QStringLiteral("fields")] = fields;
    return out;
}
