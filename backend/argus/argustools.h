#pragma once

#include <QObject>
#include <QProcess>
#include <QStringList>
#include <QJsonObject>

namespace Argus {

/* The bridge to the argus-tools binary (the Rust crate in argus-tools/): it holds the logic for
 * firmware packages, FAP manifests, snapshots, the capture library and Limbo rebuilds, and
 * answers every command with one JSON object on stdout.
 *
 * The binary is looked up in this order: $ARGUS_TOOLS, next to the running executable, on
 * PATH, then ../argus-tools/target/release next to the executable (a shadow build in <repo>/build). */

class ArgusTools
{
public:
    static QString program();
    static bool isAvailable();

    // ~/.local/share/argus (snapshots/, captures/, apps/, staging/)
    static QString dataDir();
    static QString snapshotsDir();
    static QString captureLibraryDir();
    static QString appsDir();
    // A fresh, private (0700) folder under dataDir()/staging; the caller removes it.
    static QString newStagingDir(const QString &tag);

    // Run and wait (for short commands: the Sarina API, listings). On failure the object is
    // {"ok":false,"error":{"code","message"}}.
    static QJsonObject runSync(const QStringList &args, int timeoutMs = 15000);

    static QString errorText(const QJsonObject &result);
};

/* One asynchronous argus-tools run. Emits progress() for each {"progress": "..."} line on
 * stderr and finished() once with the parsed result. Deletes itself after finished(). */
class ArgusToolsJob : public QObject
{
    Q_OBJECT

public:
    ArgusToolsJob(const QStringList &args, QObject *parent = nullptr);
    void start(int timeoutMs = 0);
    void cancel();

signals:
    void progress(const QString &message);
    void finished(const QJsonObject &result);

private:
    void finish(const QJsonObject &result);

    QProcess m_process;
    QStringList m_args;
    QByteArray m_stderrBuffer;
    bool m_done;
};

}
