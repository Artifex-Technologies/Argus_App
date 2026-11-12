#pragma once

#include <QObject>

#include "failable.h"

class QDir;
class QFile;

class TarArchive;

class TarZipArchive : public QObject, public Failable
{
    Q_OBJECT

public:
    TarZipArchive(QFile *inputFile, QObject *parent = nullptr);
    // Refuses an archive whose tar is larger than maxTarSize once uncompressed.
    TarZipArchive(QFile *inputFile, qint64 maxTarSize, QObject *parent = nullptr);
    TarZipArchive(const QDir &inputDir, QFile *outputFile, QObject *parent = nullptr);
    ~TarZipArchive();

    TarArchive *archiveIndex() const;

signals:
    void ready();

private:
    QFile *m_tarFile;
    TarArchive *m_tarArchive;
};

