#include "tarziparchive.h"

#include <QDir>
#include <QFile>

#include "tararchive.h"
#include "gzipcompressor.h"
#include "gzipuncompressor.h"
#include "tempdirectories.h"

TarZipArchive::TarZipArchive(QFile *inputFile, QObject *parent):
    TarZipArchive(inputFile, GZipUncompressor::DefaultMaxOutput, parent)
{}

TarZipArchive::TarZipArchive(QFile *inputFile, qint64 maxTarSize, QObject *parent):
    QObject(parent),
    // The tar goes to a new file in the app's temporary folder. It used to be "<base name>.tar"
    // next to the input, which replaced (and on destruction deleted) a file of that name there.
    m_tarFile(globalTempDirs->createTempFile(this)),
    m_tarArchive(nullptr)
{
    auto *uncompressor = new GZipUncompressor(inputFile, m_tarFile, maxTarSize, this);

    if(uncompressor->isError()) {
        setError(uncompressor->error(), QStringLiteral("Failed to uncompress *tar.gz file: %1").arg(uncompressor->errorString()));
        return;
    }

    connect(uncompressor, &GZipUncompressor::finished, this, [=]() {
        if(uncompressor->isError()) {
            setError(uncompressor->error(), QStringLiteral("Failed to uncompress *tar.gz file: %1").arg(uncompressor->errorString()));

        } else {
            m_tarArchive = new TarArchive(m_tarFile, this);

            if(m_tarArchive->isError()) {
                setError(m_tarArchive->error(), QStringLiteral("Failed to build archive index: %1").arg(m_tarArchive->errorString()));
            }
        }

        emit ready();
    });
}

TarZipArchive::TarZipArchive(const QDir &inputDir, QFile *outputFile, QObject *parent):
    QObject(parent),
    m_tarFile(globalTempDirs->createTempFile(this)),
    m_tarArchive(new TarArchive(inputDir, m_tarFile, this))
{
    if(m_tarArchive->isError()) {
        setError(m_tarArchive->error(), m_tarArchive->errorString());
        return;
    }

    connect(m_tarArchive, &TarArchive::ready, this, [=]() {
        auto *compressor = new GZipCompressor(m_tarFile, outputFile, this);

        if(compressor->isError()) {
            setError(compressor->error(), QStringLiteral("Failed to compress *tar.gz file: %1").arg(compressor->errorString()));
            emit ready();
            return;
        }

        connect(compressor, &GZipCompressor::finished, this, [=]() {
            if(compressor->isError()) {
                setError(compressor->error(), QStringLiteral("Failed to compress *tar.gz file: %1").arg(compressor->errorString()));
            }

            emit ready();
        });
    });
}

TarZipArchive::~TarZipArchive()
{
    m_tarFile->remove();
}

TarArchive *TarZipArchive::archiveIndex() const
{
    return m_tarArchive;
}
