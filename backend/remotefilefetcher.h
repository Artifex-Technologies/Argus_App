#pragma once

#include <QObject>

#include "failable.h"
#include "flipperupdates.h"

class QNetworkAccessManager;

class RemoteFileFetcher : public QObject, public Failable
{
    Q_OBJECT

public:
    RemoteFileFetcher(QObject *parent = nullptr);
    RemoteFileFetcher(const QString &remoteUrl, QIODevice *outputFile, QObject *parent = nullptr);
    RemoteFileFetcher(const Flipper::Updates::FileInfo &fileInfo, QIODevice *outputFile, QObject *parent = nullptr);

    bool fetch(const QString &remoteUrl, QIODevice *outputFile);
    bool fetch(const Flipper::Updates::FileInfo &fileInfo, QIODevice *outputFile);

    // The largest reply taken (a larger one is an error, never a cut file). Set before fetch().
    void setMaxSize(qint64 maxSize);

    static constexpr qint64 MAX_FILE = 64 * 1024 * 1024;

signals:
    void progressChanged(double);
    void finished();

private slots:
    void onDownloadProgress(qint64 received, qint64 total);

private:
    void checkFile(QIODevice *outputFile);
    bool fetchTestFile(const QString &remoteUrl, QIODevice *outputFile);

    QNetworkAccessManager *m_manager;
    QByteArray m_expectedChecksum;
    qint64 m_maxSize;
    qint64 m_received;
};
