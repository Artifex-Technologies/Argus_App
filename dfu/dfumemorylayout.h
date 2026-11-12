#ifndef DFUMEMORYLAYOUT_H
#define DFUMEMORYLAYOUT_H

#include <QList>
#include <QString>
#include <QByteArray>

class DFUMemoryLayout
{
public:
    struct PageBank {
        size_t pageSize;
        size_t pageCount;
        char type;
    };

    static DFUMemoryLayout fromStringDescriptor(const QByteArray &desc);

    // False when the descriptor could not be read: then there are no banks and no page addresses.
    bool isValid() const;

    const QString &name() const;
    uint32_t address() const;
    const QList<PageBank> &pageBanks() const;

    // One past the last address the layout covers.
    quint64 end() const;

    // The addresses to erase for start..end: empty if the range is empty or not inside the layout.
    const QList<uint32_t> pageAddresses(uint32_t start, quint64 end) const;


private:
    QString m_name;
    uint32_t m_address = 0;
    QList<PageBank> m_pageBanks;
};

#endif // DFUMEMORYLAYOUT_H
