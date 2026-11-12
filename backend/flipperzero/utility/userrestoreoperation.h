#pragma once

#include "abstractutilityoperation.h"

#include <QUrl>
#include <QHash>

#include "filenode.h"

class TarZipArchive;

namespace Flipper {
namespace Zero {

class UserRestoreOperation : public AbstractUtilityOperation
{
    Q_OBJECT

    enum State {
        ReadingArchive = AbstractOperation::User,
        ReadingDeviceTree,
        WritingFiles
    };

public:
    // Limits for a backup of /int (256 KiB on a Flipper Zero), the same as the Rust port's
    // flipper_rpc::restore::ArchiveLimits.
    static constexpr qint64 MaxTarSize = 80LL * 1024 * 1024;
    static constexpr qint64 MaxFileSize = 16LL * 1024 * 1024;
    static constexpr qint64 MaxTotalSize = 64LL * 1024 * 1024;
    static constexpr int MaxEntries = 10000;

    UserRestoreOperation(ProtobufSession *rpc, DeviceState *deviceState, const QUrl &backupUrl, QObject *parent = nullptr);
    const QString description() const override;

    // Checks a backup's index (already read by TarArchive, whose rules hold for any archive)
    // for what a backup of /int must also be: everything below one int/ folder, ASCII names,
    // within the limits above. Fills `entries` (int itself left out) breadth first, each folder's
    // entries in name order. Returns an empty string or why the backup is refused.
    static QString checkBackup(FileNode *root, FileNode::FileInfoList &entries);

private slots:
    void nextStateLogic() override;

private:
    void readArchive();
    void readDeviceTree();
    void writeFiles();

    QUrl m_backupUrl;
    QByteArray m_remoteDirName;
    TarZipArchive *m_archive;
    FileNode::FileInfoList m_entries;
    // Device paths below /int and whether each is a folder.
    QHash<QByteArray, bool> m_deviceTree;
};

}
}
