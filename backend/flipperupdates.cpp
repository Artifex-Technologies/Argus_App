#include "flipperupdates.h"

#include <stdexcept>

#include <algorithm>
#include <limits>

#include <QDateTime>
#include <QJsonArray>
#include <QJsonObject>
#include <QRegularExpression>

using namespace Flipper::Updates;

FileInfo::FileInfo():
    m_isValid(false)
{}

FileInfo::FileInfo(const QJsonValue &val):
    FileInfo()
{
    if(!val.isObject()) {
        throw std::runtime_error("Expected FileInfo to be an object");
    }

    const auto &json = val.toObject();
    m_isValid = json.contains("target") && json.contains("type") &&
                              json.contains("url") && json.contains("sha256");

    if(!m_isValid) {
        throw std::runtime_error("Malformed FileInfo");
    }

    m_target = json["target"].toString();
    m_type = json["type"].toString();
    m_url = json["url"].toString();
    m_sha256 = json["sha256"].toString().toLocal8Bit().toLower();

    // A file must be checkable and come from an HTTPS server: RemoteFileFetcher skipped the check for an
    // empty hash, and Qt opens file:, data: and qrc: URLs as well (docs/RUST_PORT.md; flipper_updates::directory).
    static const QRegularExpression sha256Pattern(QStringLiteral("^[0-9a-f]{64}$"));
    if(!sha256Pattern.match(QString::fromLatin1(m_sha256)).hasMatch()) {
        m_isValid = false;
        throw std::runtime_error("FileInfo: sha256 is not 64 hex digits");
    } else if(!m_url.startsWith(QStringLiteral("https://"))) {
        m_isValid = false;
        throw std::runtime_error("FileInfo: url is not https");
    }
}

const QString &FileInfo::target() const
{
    return m_target;
}

const QString &FileInfo::type() const
{
    return m_type;
}

const QString &FileInfo::url() const
{
    return m_url;
}

const QByteArray &FileInfo::sha256() const
{
    return m_sha256;
}

bool FileInfo::isValid() const
{
    return m_isValid;
}

VersionInfo::VersionInfo(const QJsonValue &val)
{
    if(!val.isObject()) {
        throw std::runtime_error("Expected VersionInfo to be an object");
    }

    const auto &json = val.toObject();
    const auto canConstruct = json.contains("version") && json.contains("changelog") &&
                              json.contains("timestamp") && json.contains("files");

    if(!canConstruct) {
        throw std::runtime_error("Malformed VersionInfo");
    } else if(!json["files"].isArray()) {
        throw std::runtime_error("VersionInfo: Expected an array of files");
    } else {}

    m_number = json["version"].toString();
    m_changelog = json["changelog"].toString();
    m_date = QDateTime::fromSecsSinceEpoch(json["timestamp"].toVariant().toULongLong()).date();

    const auto &filesArray = json["files"].toArray();
    for(const auto &file : filesArray) {
        m_files.append(FileInfo(file));
    }
}

const QString &VersionInfo::number() const
{
    return m_number;
}

const QString &VersionInfo::changelog() const
{
    return m_changelog;
}

const QDate &VersionInfo::date() const
{
    return m_date;
}

const QVector<FileInfo> &VersionInfo::files() const
{
    return m_files;
}

const FileInfo VersionInfo::fileInfo(const QString &type, const QString &target) const
{
    const auto it = std::find_if(m_files.cbegin(), m_files.cend(),
        [&](const Updates::FileInfo &arg) {
            return (arg.type() == type) && (target == arg.target());
        });

    return (it != m_files.cend()) ? *it : FileInfo();
}

qint64 VersionInfo::compare(const VersionInfo &other) const
{
    return compare(m_number, other.m_number);
}

qint64 VersionInfo::toNumericValue(const QString &version)
{
    // Each part is 1 to 9 ASCII digits, read as base-1000 digits into 64 bits. Anything else (a git hash on
    // the dev channel), or a number that does not fit, is the lowest value. (The parts used to be read with
    // toInt into an int, which overflowed for versions like 100.0.0.0.)
    static constexpr auto invalid = std::numeric_limits<qint64>::min();
    qint64 ret = 0;

    // Get rid of the possible -rcxx suffix
    const auto tokens = version.split('-').first().split('.');
    for(const auto &token : tokens) {
        const auto isDigits = std::all_of(token.cbegin(), token.cend(), [](QChar c) {
            return c >= QLatin1Char('0') && c <= QLatin1Char('9');
        });

        if(token.isEmpty() || token.size() > 9 || !isDigits) {
            return invalid;
        }

        const auto val = token.toLongLong();

        if(ret > (std::numeric_limits<qint64>::max() - val) / 1000) {
            return invalid;
        }

        ret = ret * 1000 + val;
    }

    return ret;
}

// TODO: Handle -rcxx suffixes correctly
// The sign only: subtracting overflowed when one version was not a number, which also broke the sort below.
qint64 VersionInfo::compare(const QString &v1, const QString &v2)
{
    const auto a = toNumericValue(v1), b = toNumericValue(v2);
    return (a > b) - (a < b);
}

ChannelInfo::ChannelInfo(const QJsonValue &val)
{
    if(!val.isObject()) {
        throw std::runtime_error("Expected ChannelInfo to be an object");
    }

    const auto &json = val.toObject();
    const auto canConstruct = json.contains("id") && json.contains("title") &&
                              json.contains("description") && json.contains("versions");

    if(!canConstruct) {
        throw std::runtime_error("Malformed ChannelInfo");
    } else if(!json["versions"].isArray()) {
        throw std::runtime_error("ChannelInfo: Expected an array of versions");
    } else {}

    m_id = json["id"].toString();
    m_title = json["title"].toString();
    m_description = json["description"].toString();

    const auto &versionArray = json["versions"].toArray();
    for(const auto &version : versionArray) {
        m_versions.append(VersionInfo(version));
    }

    // Json data is not guaranteed to be sorted? Equal versions keep the directory's order.
    std::stable_sort(m_versions.begin(), m_versions.end(), [](const VersionInfo &a, const VersionInfo &b) {
        return a.compare(b) > 0;
    });
}

const QString &ChannelInfo::name() const
{
    return m_id;
}

const QString &ChannelInfo::title() const
{
    return m_title;
}

const QString &ChannelInfo::description() const
{
    return m_description;
}

const QVector<VersionInfo> &ChannelInfo::versions() const
{
    return m_versions;
}

const VersionInfo &ChannelInfo::latestVersion() const
{
    // A channel without versions, or one the directory does not have (UpdateRegistry::channel), has no
    // latest version: first() on the empty list read past it.
    static const VersionInfo none;
    return m_versions.isEmpty() ? none : m_versions.first();
}
