#ifndef DFUSEFILE_H
#define DFUSEFILE_H

#include <QList>
#include <QIODevice>
#include <QByteArray>

class DfuseFile
{
public:
    struct Prefix {
        QByteArray szSignature;
        uint32_t DFUImageSize;
        uint8_t bVersion;
        uint8_t bTargets;
    };

    struct Suffix {
        uint16_t bcdDevice;
        uint16_t idProduct;
        uint16_t idVendor;
        uint16_t bcdDFU;
        uint32_t ucDFUSignature;
        uint8_t bLength;
        uint32_t dwCRC;
    };

    struct Image {
        struct TargetPrefix {
            QByteArray szSignature;
            uint8_t bAlternateSetting;
            uint32_t bTargetNamed; // a 32-bit field in the file (was a bool, and 4 bytes were read into it)
            QByteArray szTargetName;
            uint32_t dwtargetSize;
            uint32_t dwNbElements;
        };

        struct Element {
            uint32_t dwElementAddress;
            uint32_t dwElementSize;
            QByteArray data;
        };

        TargetPrefix prefix;
        QList<Element> elements;
    };

    // The largest file read (the Flipper's flash is 1 MiB). Same as the Rust port's flipper_dfu::dfuse::MAX_FILE.
    static const qint64 MaxFileSize = 16 * 1024 * 1024;

    DfuseFile(QIODevice *file);

    bool isValid() const;

    const Prefix &prefix() const;
    const Suffix &suffix() const;
    QList<Image> &images();

private:
    bool parse(const QByteArray &data);
    bool readImage(const QByteArray &data, qint64 &pos, qint64 imageEnd);
    bool checkOverlaps() const;

    static uint32_t generateCRC(const char *data, qint64 size);

    bool m_isValid;
    Prefix m_prefix;
    Suffix m_suffix;

    QList<Image> m_images;
};

#endif // DFUSEFILE_H
