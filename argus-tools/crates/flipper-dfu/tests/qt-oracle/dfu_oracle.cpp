// The C++ app's DfuSe pieces, for differential tests of the Rust port (tests/qt_oracle.rs). Built by
// build.sh from the app's own dfu/ sources (QtCore only).
//
//   dfu_oracle file PATH...   one line per file: "bad", or "ok" and every field DfuseFile read
//   dfu_oracle layout         stdin lines "DESC_HEX\tSTART_HEX\tEND_HEX": the layout DFUMemoryLayout read
//                             from DESC and the page addresses it gives for START..END
#include <QCoreApplication>
#include <QCryptographicHash>
#include <QFile>
#include <QLoggingCategory>
#include <QTextStream>

#include <cstdio>
#include <iostream>
#include <string>

#include "dfusefile.h"
#include "dfumemorylayout.h"

static QByteArray hex(const QByteArray &b)
{
    return b.isEmpty() ? QByteArray("-") : b.toHex();
}

static int fileMode(int argc, char *argv[])
{
    QTextStream out(stdout);
    for(int i = 2; i < argc; ++i) {
        QFile f(QString::fromLocal8Bit(argv[i]));
        if(!f.open(QIODevice::ReadOnly)) {
            return 2;
        }
        DfuseFile d(&f);
        if(!d.isValid()) {
            out << "bad\n";
            continue;
        }
        const auto &s = d.suffix();
        out << "ok v=" << d.prefix().bVersion << " dev=" << s.bcdDevice << " pid=" << s.idProduct << " vid=" << s.idVendor
            << " dfu=" << s.bcdDFU << " crc=" << s.dwCRC;
        for(const auto &img : d.images()) {
            out << " | alt=" << img.prefix.bAlternateSetting << " named=" << (quint64)img.prefix.bTargetNamed
                << " name=" << hex(img.prefix.szTargetName) << " size=" << img.prefix.dwtargetSize << " n=" << img.elements.size();
            for(const auto &e : img.elements) {
                out << " [" << e.dwElementAddress << " " << e.dwElementSize << " " << e.data.size() << " "
                    << QCryptographicHash::hash(e.data, QCryptographicHash::Md5).toHex() << "]";
            }
        }
        out << '\n';
    }
    return 0;
}

static int layoutMode()
{
    QTextStream out(stdout);
    std::string line;
    while(std::getline(std::cin, line)) {
        const auto fields = QByteArray::fromStdString(line).split('\t');
        if(fields.size() != 3) {
            return 2;
        }
        const auto desc = QByteArray::fromHex(fields.at(0) == "-" ? QByteArray() : fields.at(0));
        const auto layout = DFUMemoryLayout::fromStringDescriptor(desc);
        if(!layout.isValid()) {
            out << "bad\n";
            out.flush();
            continue;
        }
        out << "ok name=" << hex(layout.name().toUtf8()) << " addr=" << layout.address() << " banks=";
        for(const auto &b : layout.pageBanks()) {
            out << b.pageCount << '*' << b.pageSize << (char)b.type << ',';
        }
        const auto pages = layout.pageAddresses(fields.at(1).toUInt(nullptr, 16), fields.at(2).toULongLong(nullptr, 16));
        out << " pages=";
        if(pages.isEmpty()) {
            out << "none";
        }
        for(const auto p : pages) {
            out << p << ',';
        }
        out << '\n';
        out.flush();
    }
    return 0;
}

int main(int argc, char *argv[])
{
    QCoreApplication app(argc, argv);
    // the parsers' own messages go to stderr; keep them out of the way
    QLoggingCategory::setFilterRules(QStringLiteral("DBG.debug=false"));
    const QByteArray mode = argc > 1 ? argv[1] : "";
    if(mode == "file") {
        return fileMode(argc, argv);
    } else if(mode == "layout") {
        return layoutMode();
    }
    std::fprintf(stderr, "usage: dfu_oracle file PATH... | layout\n");
    return 2;
}
