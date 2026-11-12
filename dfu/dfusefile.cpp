#include "dfusefile.h"

#include <QDebug>
#include <QtEndian>

#include <algorithm>
#include <tuple>

#include "debug.h"

// The file is read whole (at most MaxFileSize) and every length is checked against the bytes that are
// there before anything is read or allocated. Before, a file claiming four billion elements made the
// parser append empty elements until it ran out of memory, and a target, an element or the suffix that
// was cut off was read anyway. The same rules as the Rust port (argus-tools/crates/flipper-dfu,
// src/dfuse.rs); tests/qt_oracle.rs there checks that both accept and read the same files.

static const auto DFUSE_PREFIX_SIZE = 11;
static const auto DFUSE_TARGET_PREFIX_SIZE = 274;
static const auto DFUSE_ELEMENT_HEADER_SIZE = 8;
static const auto DFUSE_SUFFIX_SIZE = 16;

static uint16_t u16At(const QByteArray &data, qint64 pos)
{
    return qFromLittleEndian<uint16_t>(data.constData() + pos);
}

static uint32_t u32At(const QByteArray &data, qint64 pos)
{
    return qFromLittleEndian<uint32_t>(data.constData() + pos);
}

DfuseFile::DfuseFile(QIODevice *file):
    m_isValid(false),
    m_prefix(),
    m_suffix()
{
    const auto size = file->size();
    check_return_void(size <= MaxFileSize, "DfuSe file is too large");

    const auto data = file->read(MaxFileSize + 1);
    check_return_void(data.size() == size, "Failed to read the DfuSe file");

    m_isValid = parse(data);

    if(!m_isValid) {
        m_images.clear();
    }
}

bool DfuseFile::isValid() const
{
    return m_isValid;
}

const DfuseFile::Prefix &DfuseFile::prefix() const
{
    return m_prefix;
}

const DfuseFile::Suffix &DfuseFile::suffix() const
{
    return m_suffix;
}

QList<DfuseFile::Image> &DfuseFile::images()
{
    return m_images;
}

bool DfuseFile::parse(const QByteArray &data)
{
    const qint64 size = data.size();
    check_return_bool(size >= DFUSE_PREFIX_SIZE + DFUSE_SUFFIX_SIZE, "File is too short");

    m_prefix.szSignature = data.left(5);
    check_return_bool(m_prefix.szSignature == "DfuSe", "Not a valid DfuSe file");

    m_prefix.bVersion = (uint8_t)data.at(5);
    m_prefix.DFUImageSize = u32At(data, 6);
    m_prefix.bTargets = (uint8_t)data.at(10);

    const auto imageEnd = size - DFUSE_SUFFIX_SIZE;

    check_return_bool(m_prefix.bVersion == 1, "Unknown DfuSe version");
    check_return_bool(m_prefix.DFUImageSize == imageEnd, "File size does not match");
    check_return_bool(m_prefix.bTargets > 0, "The file has no target");

    qint64 pos = DFUSE_PREFIX_SIZE;

    for(auto i = 0; i < m_prefix.bTargets; ++i) {
        check_return_bool(readImage(data, pos, imageEnd), "Failed to read DfuSe image");
    }

    check_return_bool(pos == imageEnd, "Unexpected bytes after the last target");

    m_suffix.bcdDevice = u16At(data, imageEnd);
    m_suffix.idProduct = u16At(data, imageEnd + 2);
    m_suffix.idVendor = u16At(data, imageEnd + 4);
    m_suffix.bcdDFU = u16At(data, imageEnd + 6);
    m_suffix.ucDFUSignature = u32At(data, imageEnd + 8) & 0xffffffU;
    m_suffix.bLength = (uint8_t)data.at(imageEnd + 11);
    m_suffix.dwCRC = u32At(data, imageEnd + 12);

    check_return_bool((m_suffix.ucDFUSignature == 0x444655) && (m_suffix.bLength == DFUSE_SUFFIX_SIZE), "No valid DfuSe suffix");
    check_return_bool(m_suffix.dwCRC == generateCRC(data.constData(), size - 4), "Checksum doesn't match");
    check_return_bool(checkOverlaps(), "DfuSe elements overlap");

    return true;
}

bool DfuseFile::readImage(const QByteArray &data, qint64 &pos, qint64 imageEnd)
{
    const auto TARGET_SIGNATURE_SIZE = 6;
    const auto TARGET_NAME_SIZE = 255;

    check_return_bool(imageEnd - pos >= DFUSE_TARGET_PREFIX_SIZE, "DfuSe target is cut off");

    Image img;

    img.prefix.szSignature = data.mid(pos, TARGET_SIGNATURE_SIZE);
    check_return_bool(img.prefix.szSignature == "Target", "Not a valid DfuSe target image");

    img.prefix.bAlternateSetting = (uint8_t)data.at(pos + 6);
    img.prefix.bTargetNamed = u32At(data, pos + 7);
    img.prefix.szTargetName = data.mid(pos + 11, TARGET_NAME_SIZE);
    img.prefix.dwtargetSize = u32At(data, pos + 266);
    img.prefix.dwNbElements = u32At(data, pos + 270);

    pos += DFUSE_TARGET_PREFIX_SIZE;

    // every element takes at least its header
    check_return_bool(img.prefix.dwNbElements <= (quint64)(imageEnd - pos) / DFUSE_ELEMENT_HEADER_SIZE,
                      "DfuSe target claims more elements than the file holds");

    const auto start = pos;

    for(uint32_t i = 0; i < img.prefix.dwNbElements; ++i) {
        check_return_bool(imageEnd - pos >= DFUSE_ELEMENT_HEADER_SIZE, "DfuSe element is cut off");

        Image::Element el;
        el.dwElementAddress = u32At(data, pos);
        el.dwElementSize = u32At(data, pos + 4);
        pos += DFUSE_ELEMENT_HEADER_SIZE;

        check_return_bool(el.dwElementSize <= (quint64)(imageEnd - pos), "DfuSe element is cut off");
        // an empty element used to erase a whole page and write nothing back
        check_return_bool(el.dwElementSize > 0, "DfuSe element is empty");
        check_return_bool((quint64)el.dwElementAddress + el.dwElementSize <= (Q_UINT64_C(1) << 32),
                          "DfuSe element runs past the end of the address space");

        el.data = data.mid(pos, el.dwElementSize);
        pos += el.dwElementSize;

        img.elements.append(el);
    }

    check_return_bool((quint64)(pos - start) == img.prefix.dwtargetSize, "DfuSe target size does not match its elements");

    m_images.append(img);
    return true;
}

// Two elements for the same alternate setting must not overlap: which bytes the device kept would
// depend on the order of the writes.
bool DfuseFile::checkOverlaps() const
{
    QList<std::tuple<uint8_t, quint64, quint64>> spans;

    for(const auto &img : m_images) {
        for(const auto &el : img.elements) {
            spans.append({img.prefix.bAlternateSetting, el.dwElementAddress, (quint64)el.dwElementAddress + el.dwElementSize});
        }
    }

    std::sort(spans.begin(), spans.end());

    for(auto i = 1; i < spans.size(); ++i) {
        const auto &a = spans.at(i - 1);
        const auto &b = spans.at(i);
        check_return_bool(std::get<0>(a) != std::get<0>(b) || std::get<1>(b) >= std::get<2>(a), "DfuSe elements overlap");
    }

    return true;
}

uint32_t DfuseFile::generateCRC(const char *data, qint64 size)
{
    const auto LUT_SIZE = 256;
    uint32_t lut[LUT_SIZE];

    for(auto i = 0; i < LUT_SIZE; ++i) {
        uint32_t val = i;

        for(auto j = 0; j < 8; ++j) {
            val = (val & 1U) ? 0xEDB88320U ^ (val >> 1) : val >> 1;
        }

        lut[i] = val;
    }

    uint32_t val = UINT32_MAX;

    for(qint64 i = 0; i < size; ++i) {
        const auto c = (uint8_t)data[i];
        val = lut[(val ^ c) & UINT8_MAX] ^ (val >> 8);
    }

    return val;
}
