#pragma once

#include <QObject>

#include "failable.h"

class QIODevice;

class GZipUncompressor : public QObject, public Failable
{
    Q_OBJECT

public:
    // Output beyond this many bytes fails the uncompression (a small file can expand to any size).
    static constexpr qint64 DefaultMaxOutput = 1024LL * 1024 * 1024;

    GZipUncompressor(QIODevice *in, QIODevice* out, QObject *parent = nullptr);
    GZipUncompressor(QIODevice *in, QIODevice* out, qint64 maxOutput, QObject *parent = nullptr);
    ~GZipUncompressor();

    double progress() const;

signals:
    void finished();
    void progressChanged();

private:
    void setProgress(double progress);
    void doUncompress();
    void closeFiles();

    QIODevice *m_in;
    QIODevice *m_out;

    double m_progress;
    qint64 m_maxOutput;
};


