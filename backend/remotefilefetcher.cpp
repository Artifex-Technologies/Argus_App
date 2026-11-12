#include "remotefilefetcher.h"

#include <QNetworkAccessManager>
#include <QCryptographicHash>
#include <QNetworkReply>
#include <QTimer>
#include <QFile>
#include <QDir>
#include <QUrl>

#include "debug.h"
#include "testhooks.h"

using namespace Flipper;

RemoteFileFetcher::RemoteFileFetcher(QObject *parent):
    QObject(parent),
    m_manager(new QNetworkAccessManager(this))
{}

RemoteFileFetcher::RemoteFileFetcher(const QString &remoteUrl, QIODevice *outputFile, QObject *parent):
    RemoteFileFetcher(parent)
{
    fetch(remoteUrl, outputFile);
}

RemoteFileFetcher::RemoteFileFetcher(const Flipper::Updates::FileInfo &fileInfo, QIODevice *outputFile, QObject *parent):
    RemoteFileFetcher(parent)
{
    fetch(fileInfo, outputFile);
}

bool RemoteFileFetcher::fetch(const QString &remoteUrl, QIODevice *outputFile)
{
    if(!outputFile->open(QIODevice::WriteOnly)) {
        setError(BackendError::DiskError, QStringLiteral("Failed to open file for writing: %1.").arg(outputFile->errorString()));
        return false;
    }

    if(Argus::isTestDevice()) {
        return fetchTestFile(remoteUrl, outputFile);
    }

    auto *reply = m_manager->get(QNetworkRequest(remoteUrl));

    if(reply->error() != QNetworkReply::NoError) {
        setError(BackendError::InternetError, QStringLiteral("Network error: %1").arg(reply->errorString()));

        reply->deleteLater();
        return false;
    }

    const auto onReplyReadyRead = [=]() {
        outputFile->write(reply->readAll());
    };

    connect(reply, &QNetworkReply::finished, this, [=]() {
        // In case there was any leftover data
        onReplyReadyRead();

        outputFile->close();
        reply->deleteLater();

        if(reply->error() != QNetworkReply::NoError) {
            setError(BackendError::InternetError, QStringLiteral("Network error: %1").arg(reply->errorString()));
        } else {
            checkFile(outputFile);
        }

        emit finished();
    });

    connect(reply, &QNetworkReply::readyRead, this, onReplyReadyRead);
    connect(reply, &QNetworkReply::downloadProgress, this, &RemoteFileFetcher::onDownloadProgress);

    return true;
}

void RemoteFileFetcher::checkFile(QIODevice *outputFile)
{
    if(m_expectedChecksum.isEmpty()) {
        return;
    }

    if(!outputFile->open(QIODevice::ReadOnly)) {
        setError(BackendError::DiskError, QStringLiteral("Failed to open file for reading: %1.").arg(outputFile->errorString()));
        return;
    }

    QCryptographicHash hash(QCryptographicHash::Sha256);
    hash.addData(outputFile);

    if(hash.result().toHex() != m_expectedChecksum) {
        setError(BackendError::UnknownError, QStringLiteral("File integrity check failed"));
    }

    outputFile->close();
}

// Test hook (testhooks.h): the file named by the URL comes from ARGUS_TEST_FIRMWARE_DIR, never from the
// network; without that folder every fetch fails. Finishes from the event loop, as a reply would.
bool RemoteFileFetcher::fetchTestFile(const QString &remoteUrl, QIODevice *outputFile)
{
    static constexpr qint64 MAX_TEST_FILE = 64 * 1024 * 1024;

    const auto dir = Argus::testFirmwareDir();
    const auto name = QUrl(remoteUrl).fileName();

    QString error;

    if(dir.isEmpty()) {
        error = QStringLiteral("no network in tests (ARGUS_TEST_FIRMWARE_DIR is not set)");
    } else if(name.isEmpty() || name.startsWith('.') || name.contains('/') || name.contains('\\')) {
        error = QStringLiteral("unusable file name in %1").arg(remoteUrl);
    } else {
        QFile file(QDir(dir).filePath(name));

        if(!file.open(QIODevice::ReadOnly)) {
            error = QStringLiteral("%1: %2").arg(file.fileName(), file.errorString());
        } else if(file.size() > MAX_TEST_FILE) {
            error = QStringLiteral("%1 is too large").arg(file.fileName());
        } else if(outputFile->write(file.readAll()) != file.size()) {
            error = QStringLiteral("Failed to write the file: %1").arg(outputFile->errorString());
        } else {}
    }

    outputFile->close();

    QTimer::singleShot(0, this, [=]() {
        if(!error.isEmpty()) {
            setError(BackendError::InternetError, QStringLiteral("Test firmware folder: %1").arg(error));
        } else {
            checkFile(outputFile);
        }

        emit finished();
    });

    return true;
}

bool RemoteFileFetcher::fetch(const Flipper::Updates::FileInfo &fileInfo, QIODevice *outputFile)
{
    m_expectedChecksum = fileInfo.sha256();
    return fetch(fileInfo.url(), outputFile);
}

void RemoteFileFetcher::onDownloadProgress(qint64 received, qint64 total)
{
    emit progressChanged(((double)received / (double)total) * 100.0);
}
