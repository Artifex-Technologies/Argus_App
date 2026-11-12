// The C++ app's DfuSe pieces, for differential tests of the Rust port (tests/qt_oracle.rs). Built by
// build.sh from the app's own dfu/ sources (QtCore only).
//
//   dfu_oracle file PATH...   one line per file: "bad", or "ok" and every field DfuseFile read
//   dfu_oracle layout         stdin lines "DESC_HEX\tSTART_HEX\tEND_HEX": the layout DFUMemoryLayout read
//                             from DESC and the page addresses it gives for START..END
//   dfu_oracle device OP ...  one DfuseDevice operation over fakeusbdevice.cpp (every USB request is a line
//                             on stdout, answered on stdin), then "RESULT ok|fail DATA_HEX":
//                             flash FILE | erase ADDR SIZE | download ADDR ALT FILE | upload ADDR SIZE ALT |
//                             command HEX | leave | origin ALT   (ADDR in hex, SIZE in decimal)
#include <QBuffer>
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
#include "dfusedevice.h"

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

static int deviceMode(int argc, char *argv[])
{
    const QByteArray op = argc > 2 ? argv[2] : "";
    auto arg = [&](int i) { return i < argc ? QByteArray(argv[i]) : QByteArray(); };

    DfuseDevice dev((USBDeviceInfo()));
    bool ok = dev.beginTransaction();
    QByteArray out;

    if(!ok) {
    } else if(op == "flash") {
        QFile f(QString::fromLocal8Bit(arg(3)));
        ok = f.open(QIODevice::ReadOnly);
        if(ok) {
            DfuseFile fw(&f);
            ok = dev.download(&fw);
        }
    } else if(op == "erase") {
        ok = dev.erase(arg(3).toUInt(nullptr, 16), arg(4).toULongLong());
    } else if(op == "download") {
        QFile f(QString::fromLocal8Bit(arg(5)));
        ok = f.open(QIODevice::ReadOnly);
        if(ok) {
            ok = dev.download(&f, arg(3).toUInt(nullptr, 16), (uint8_t)arg(4).toUInt());
        }
    } else if(op == "upload") {
        QBuffer buf;
        buf.open(QIODevice::WriteOnly);
        ok = dev.upload(&buf, arg(3).toUInt(nullptr, 16), arg(4).toULongLong(), (uint8_t)arg(5).toUInt());
        out = buf.data();
    } else if(op == "command") {
        ok = dev.download(QByteArray::fromHex(arg(3)));
    } else if(op == "leave") {
        ok = dev.leave();
    } else if(op == "origin") {
        out = QByteArray::number(dev.partitionOrigin((uint8_t)arg(3).toUInt()), 16);
    } else {
        return 2;
    }

    dev.endTransaction();
    std::cout << "RESULT " << (ok ? "ok " : "fail ") << hex(out).toStdString() << std::endl;
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
    } else if(mode == "device") {
        return deviceMode(argc, argv);
    }
    std::fprintf(stderr, "usage: dfu_oracle file PATH... | layout | device OP ...\n");
    return 2;
}
