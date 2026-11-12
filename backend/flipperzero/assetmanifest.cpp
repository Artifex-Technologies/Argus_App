#include "assetmanifest.h"

#include <QTextStream>

#include "debug.h"

using namespace Flipper;
using namespace Zero;

AssetManifest::AssetManifest():
    m_version(-1),
    m_timestamp(0),
    m_root(new FileNode("", FileNode::Type::Directory))
{}

AssetManifest::AssetManifest(const QByteArray &text):
    AssetManifest()
{
    QTextStream s(text);

    auto n = 1;
    while(!s.atEnd()) {
        if(!parseLine(s.readLine())) {
            setError(BackendError::DataError, QStringLiteral("Syntax error on line %1").arg(n++));
            return;
        }
    }

    if((m_version == -1) || (m_timestamp == 0)) {
        setError(BackendError::DataError, QStringLiteral("Incomplete manifest file"));
    }
}

int AssetManifest::version() const
{
    return m_version;
}

qint64 AssetManifest::timestamp() const
{
    return m_timestamp;
}

FileNode *AssetManifest::tree() const
{
    return m_root.get();
}

bool AssetManifest::parseLine(const QString &line)
{
    const auto tokens = line.trimmed().split(':');
    return parseFile(tokens) || parseDirectory(tokens) ||
            parseVersion(tokens) || parseTime(tokens);
}

bool AssetManifest::parseVersion(const QStringList &tokens)
{
    if((tokens.first() != "V") || (tokens.size() < 2)) {
        return false;
    }

    bool success;
    const auto version = tokens[1].toInt(&success);

    if(success) {
        m_version = version;
    }

    return success;
}

bool AssetManifest::parseTime(const QStringList &tokens)
{
    if((tokens.first() != "T") || (tokens.size() < 2)) {
        return false;
    }

    bool success;
    const auto time = tokens[1].toLongLong(&success, 10);

    if(success) {
        m_timestamp = time;
    }

    return success;
}

// A path that stays inside the folder the manifest describes (/ext): no empty part, ".", "..", backslash or
// control character, and its parent is a folder already listed. Such names used to be taken as they were, so
// a device's own /ext/Manifest could make the app remove /ext/../int/... (the same rule as the Rust port's
// flipper_rpc::assets).
bool AssetManifest::isAcceptablePath(const QString &path) const
{
    const auto parts = path.split('/');

    for(const auto &part : parts) {
        if(part.isEmpty() || part == QStringLiteral(".") || part == QStringLiteral("..") || part.contains('\\')) {
            return false;
        }

        for(const auto &c : part) {
            if(c.category() == QChar::Other_Control) {
                return false;
            }
        }
    }

    if(parts.size() == 1) {
        return true;
    }

    const auto *parent = m_root->find(parts.mid(0, parts.size() - 1).join('/'));
    return parent && (parent->type() == FileNode::Type::Directory);
}

bool AssetManifest::parseFile(const QStringList &tokens)
{
    if((tokens.first() != "F") || (tokens.size() < 4)) {
        return false;
    }

    bool success;
    FileInfo info;

    info.md5 = tokens[1].toLocal8Bit();
    info.size = tokens[2].toLongLong(&success, 10);

    return success && isAcceptablePath(tokens[3]) && m_root->addFile(tokens[3], QVariant::fromValue(info));
}

bool AssetManifest::parseDirectory(const QStringList &tokens)
{
    if((tokens.first() != "D") || (tokens.size() < 2)) {
        return false;
    }

    const auto dirName = tokens[1].endsWith('/') ? tokens[1].chopped(1) : tokens[1];
    return isAcceptablePath(dirName) && m_root->addDirectory(dirName);
}

bool AssetManifest::FileInfo::operator ==(const FileInfo &other) const
{
    return (size == other.size) && (md5 == other.md5);
}

bool AssetManifest::FileInfo::operator <(const FileInfo &other) const
{
    return size < other.size;
}
