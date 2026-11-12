#include "dfumemorylayout.h"

#include <QDebug>

#include "debug.h"

// The descriptor comes from the device, so anything off is refused rather than guessed at. Before, "1*K"
// read one character before the start of the page size, a number that could not be read became 0, 'M'
// was taken as bytes, the banks read before an error were kept, and pageAddresses walked every page from
// the start of the layout, so a bank of billions of empty pages hung the app. The same grammar as the Rust
// port (argus-tools/crates/flipper-dfu, src/layout.rs); tests/qt_oracle.rs there compares the two.

static bool isDecimal(const QByteArray &s)
{
    if(s.isEmpty() || s.size() > 10) {
        return false;
    }

    for(const auto c : s) {
        if(c < '0' || c > '9') {
            return false;
        }
    }

    return true;
}

static bool isHex(const QByteArray &s)
{
    if(s.isEmpty() || s.size() > 8) {
        return false;
    }

    for(const auto c : s) {
        if(!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F'))) {
            return false;
        }
    }

    return true;
}

DFUMemoryLayout DFUMemoryLayout::fromStringDescriptor(const QByteArray &desc)
{
    DFUMemoryLayout ret;

    for(const auto c : desc) {
        if((uint8_t)c >= 0x80) {
            error_msg("non-ASCII descriptor");
            return ret;
        }
    }

    const auto fields = desc.split('/');

    if(fields.size() != 3) {
        error_msg("bad descriptor syntax");
        return ret;
    }

    const auto address = fields.at(1).trimmed();

    if(!(address.startsWith("0x") || address.startsWith("0X")) || !isHex(address.mid(2))) {
        error_msg("bad address");
        return ret;
    }

    QList<PageBank> banks;
    quint64 total = address.mid(2).toULongLong(nullptr, 16);

    for(const auto &field : fields.at(2).split(',')) {
        const auto bank = field.trimmed();
        const auto star = bank.indexOf('*');

        if(star < 0 || bank.size() - star - 1 < 3) {
            error_msg("bad page bank syntax");
            return ret;
        }

        const auto pageCount = bank.left(star);
        const auto pageSize = bank.mid(star + 1, bank.size() - star - 3);
        const auto multiplierChar = bank.at(bank.size() - 2);
        const auto type = bank.at(bank.size() - 1);

        quint64 multiplier;

        if(multiplierChar == ' ' || multiplierChar == 'B') {
            multiplier = 1;
        } else if(multiplierChar == 'K') {
            multiplier = 1024;
        } else if(multiplierChar == 'M') {
            multiplier = 1024 * 1024;
        } else {
            error_msg("bad page size multiplier");
            return ret;
        }

        if(!isDecimal(pageCount) || !isDecimal(pageSize) || type < 'a' || type > 'g') {
            error_msg("bad page bank syntax");
            return ret;
        }

        const auto count = pageCount.toULongLong();
        const auto size = pageSize.toULongLong() * multiplier;

        if(count == 0 || size == 0 || count > UINT32_MAX || size > UINT32_MAX) {
            error_msg("bad page bank size");
            return ret;
        }

        total += count * size;

        if(total > (Q_UINT64_C(1) << 32)) {
            error_msg("memory layout runs past the end of the address space");
            return ret;
        }

        PageBank elem;
        elem.pageCount = count;
        elem.pageSize = size;
        elem.type = type;

        banks.append(elem);
    }

    ret.m_name = QString::fromLatin1(fields.at(0).trimmed());
    ret.m_address = (uint32_t)address.mid(2).toULong(nullptr, 16);
    ret.m_pageBanks = banks;

    return ret;
}

bool DFUMemoryLayout::isValid() const
{
    return !m_pageBanks.isEmpty();
}

const QString &DFUMemoryLayout::name() const
{
    return m_name;
}

uint32_t DFUMemoryLayout::address() const
{
    return m_address;
}

const QList<DFUMemoryLayout::PageBank> &DFUMemoryLayout::pageBanks() const
{
    return m_pageBanks;
}

quint64 DFUMemoryLayout::end() const
{
    quint64 ret = m_address;

    for(const auto &bank : m_pageBanks) {
        ret += (quint64)bank.pageCount * bank.pageSize;
    }

    return ret;
}

const QList<uint32_t> DFUMemoryLayout::pageAddresses(uint32_t start, quint64 end) const
{
    QList<uint32_t> ret;

    if(!isValid()) {
        error_msg("page banks empty");
        return ret;
    } else if((start < m_address) || ((quint64)start >= end) || (end > this->end())) {
        error_msg("address error");
        return ret;
    } else {}

    ret.append(start);

    // Page boundaries bankStart + k * pageSize (k >= 1) with start < boundary < end, computed per bank
    // instead of walking every page from the start of the layout.
    quint64 bankStart = m_address;

    for(const auto &bank : m_pageBanks) {
        const quint64 size = bank.pageSize;
        const quint64 bankEnd = bankStart + bank.pageCount * size;
        const quint64 firstPage = (start > bankStart ? start - bankStart : 0) / size + 1;

        for(auto boundary = bankStart + firstPage * size; boundary <= bankEnd && boundary < end; boundary += size) {
            ret.append((uint32_t)boundary);
        }

        if(bankEnd >= end) {
            break;
        }

        bankStart = bankEnd;
    }

    return ret;
}
