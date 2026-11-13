#include "radiomanifesthelper.h"

#include <QFile>
#include <QCryptographicHash>

#include "tararchive.h"
#include "tarziparchive.h"

#include "flipperzero/radiomanifest.h"

using namespace Flipper;
using namespace Zero;

RadioManifestHelper::RadioManifestHelper(QFile *radioArchive, QObject *parent):
    AbstractOperationHelper(parent),
    m_compressedFile(radioArchive)
{}

int RadioManifestHelper::stackType() const
{
    return m_manifest.firmware().radio().type();
}

const QString &RadioManifestHelper::radioVersion() const
{
    return m_manifest.firmware().radio().version();
}

const QString &RadioManifestHelper::fusVersion() const
{
    return m_manifest.firmware().fus().version();
}

const QByteArray RadioManifestHelper::radioFirmwareData() const
{
    const auto &fileName = m_manifest.firmware().radio().files().first().name();
    return m_archive->archiveIndex()->fileData(QStringLiteral("core2_firmware/%1").arg(fileName));
}

void RadioManifestHelper::nextStateLogic()
{
    if(state() == AbstractOperationHelper::Ready) {
        setState(RadioManifestHelper::UncompressingArchive);
        uncompressArchive();

    } else if(state() == RadioManifestHelper::UncompressingArchive) {
        setState(RadioManifestHelper::ReadingManifest);
        readManifest();

    } else if(state() == RadioManifestHelper::ReadingManifest) {
        finish();
    }
}

void RadioManifestHelper::uncompressArchive()
{
    m_archive = new TarZipArchive(m_compressedFile, this);

    if(m_archive->isError()) {
        finishWithError(m_archive->error(), QStringLiteral("Failed to uncompress archive file: %1").arg(m_archive->errorString()));
        return;
    }

    connect(m_archive, &TarZipArchive::ready, this, [=]() {
        if(m_archive->isError()) {
            finishWithError(m_archive->error(), QStringLiteral("Failed to uncompress archive file: %1").arg(m_archive->errorString()));
        } else {
            advanceState();
        }
    });
}

void RadioManifestHelper::readManifest()
{
    const auto manifext = m_archive->archiveIndex()->fileData(QStringLiteral("core2_firmware/Manifest.json"));
    m_manifest = RadioManifest(manifext);

    if(m_manifest.isError()) {
        finishWithError(m_manifest.error(), QStringLiteral("Failed to read radio manifest: %1").arg(m_manifest.errorString()));
        return;
    }

    // The image must be in the archive and be what the manifest says: only the archive as a whole was checked
    // (against the directory), so a damaged archive's image was flashed as it was. The Rust port has the same rule
    // (flipper_updates::repair::radio_image); compare-repair.sh "core2-mismatch" runs both on such an archive.
    const auto &fileInfo = m_manifest.firmware().radio().files().first();
    const auto data = radioFirmwareData();
    const auto expected = fileInfo.sha256().toLower();
    const auto got = QCryptographicHash::hash(data, QCryptographicHash::Sha256).toHex();

    if(data.isEmpty()) {
        finishWithError(BackendError::DataError, QStringLiteral("The wireless stack archive lacks %1, which its manifest names").arg(fileInfo.name()));
    } else if(expected.size() != 64 || got != expected) {
        finishWithError(BackendError::DataError, QStringLiteral("The wireless stack image %1 does not match its manifest").arg(fileInfo.name()));
    } else {
        advanceState();
    }
}
