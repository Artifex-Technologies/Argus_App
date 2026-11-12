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
    m_manager(new QNetworkAccessManager(this)),
    m_maxSize(MAX_FILE),
    m_received(0)
{}

void RemoteFileFetcher::setMaxSize(qint64 maxSize)
{
    m_maxSize = maxSize;
}

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

    // HTTPS only (Qt also opens file:, data: and qrc: URLs), a bounded time and at most 3 redirects (Qt's
    // default policy already refuses one from https to http).
    const QUrl url(remoteUrl);
    if(url.scheme() != QStringLiteral("https")) {
        outputFile->close();
        setError(BackendError::InternetError, QStringLiteral("Not an https URL: %1").arg(remoteUrl));
        return false;
    }

    QNetworkRequest request(url);
    request.setTransferTimeout(10 * 60 * 1000);
    request.setMaximumRedirectsAllowed(3);

    m_received = 0;
    auto *reply = m_manager->get(request);

    if(reply->error() != QNetworkReply::NoError) {
        setError(BackendError::InternetError, QStringLiteral("Network error: %1").arg(reply->errorString()));

        reply->deleteLater();
        return false;
    }

    // Every reply is bounded: past m_maxSize the transfer stops and the fetch fails (it used to take
    // whatever came, into memory for the update directory).
    const auto tooLarge = [=]() {
        if(!isError()) {
            setError(BackendError::InternetError, QStringLiteral("The reply from %1 is larger than %2 bytes").arg(remoteUrl).arg(m_maxSize));
        }
        reply->abort();
    };

    const auto onReplyReadyRead = [=]() {
        if(isError()) {
            reply->readAll();
            return;
        }

        const auto data = reply->readAll();
        m_received += data.size();

        if(m_received > m_maxSize) {
            tooLarge();
        } else {
            outputFile->write(data);
        }
    };

    connect(reply, &QNetworkReply::metaDataChanged, this, [=]() {
        const auto length = reply->header(QNetworkRequest::ContentLengthHeader);
        if(length.isValid() && length.toLongLong() > m_maxSize) {
            tooLarge();
        }
    });

    connect(reply, &QNetworkReply::finished, this, [=]() {
        // In case there was any leftover data
        onReplyReadyRead();

        outputFile->close();
        reply->deleteLater();

        if(isError()) {
            // too large: already said
        } else if(reply->error() != QNetworkReply::NoError) {
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
        } else if(file.size() > m_maxSize) {
            error = QStringLiteral("The reply from %1 is larger than %2 bytes").arg(remoteUrl).arg(m_maxSize);
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
    // total is -1 when the server sends no length
    if(total > 0) {
        emit progressChanged(((double)received / (double)total) * 100.0);
    }
}
