#pragma once

#include <QObject>
#include <QPointer>
#include <QStringList>

#include "fileinfo.h"

namespace Flipper {
class FlipperZero;
}

namespace Chymaera {

/* Copies capture files off a Flipper into a local staging directory, one
 * remote root at a time, then one file at a time. Read-only on the device:
 * it only lists directories and reads files over the RPC storage link.
 *
 * Unlike the upstream DirectoryDownloadOperation it skips roots that do not
 * exist, leaves out excluded sub-trees (key dictionaries, caches), finishes on
 * roots that hold no files, and mirrors the full remote path locally
 * (<staging>/ext/nfc/x.nfc) so files from different roots never collide. */

// Where a file named by the device goes in the staging folder; empty when the name would lead outside it.
QString stagedPath(const QString &stagingDir, const QString &remotePath);

class CaptureSync : public QObject
{
    Q_OBJECT

public:
    CaptureSync(Flipper::FlipperZero *device, const QStringList &remoteRoots,
                const QString &stagingDir, QObject *parent = nullptr);

    // The capture folders "pull everything" copies. Deliberately an allowlist:
    // /ext/u2f, apps_data/totp and similar hold secrets, not captures.
    static QStringList defaultRoots();

    // True for a path inside an excluded sub-tree ("assets", ".cache").
    static bool isExcluded(const QByteArray &remotePath);
    // Whether a path may be pulled at all: one of defaultRoots() or below one, after normalising, with no "..". Everything
    // else on the device (/int with its Bluetooth bonding keys, U2F and TOTP secrets) is never copied off it.
    static bool isCaptureRoot(const QString &remotePath);

    void start();

    QString stagingDir() const;
    int fileCount() const;
    int failedCount() const;
    int missingRootCount() const;
    qint64 byteCount() const;

signals:
    void rootMissing(const QString &remoteRoot, const QString &reason);
    void fileReceived(const QString &localPath, const QString &remotePath);
    void fileFailed(const QString &remotePath, const QString &reason);
    void finished(bool ok);

private:
    void listNextRoot();
    void readNextFile();
    void finish(bool ok);

    QPointer<Flipper::FlipperZero> m_device;
    QStringList m_roots;
    QString m_stagingDir;
    FileInfoList m_queue;

    int m_fileCount;
    int m_failedCount;
    int m_missingRootCount;
    qint64 m_byteCount;
    bool m_finished;
};

}
