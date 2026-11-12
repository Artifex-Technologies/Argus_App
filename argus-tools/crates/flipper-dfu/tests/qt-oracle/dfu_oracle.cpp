// The C++ app's DfuSe pieces, for differential tests of the Rust port (tests/qt_oracle.rs). Built by
// build.sh from the app's own dfu/ sources (QtCore only).
//
//   dfu_oracle file PATH...   one line per file: "bad", or "ok" and every field DfuseFile read
//   dfu_oracle layout         stdin lines "DESC_HEX\tSTART_HEX\tEND_HEX": the layout DFUMemoryLayout read
//                             from DESC and the page addresses it gives for START..END
//   dfu_oracle device OP ...  one DfuseDevice operation over fakeusbdevice.cpp (every USB request is a line
//                             on stdout, answered on stdin), then "RESULT ok|fail DATA_HEX":
//                             flash FILE | erase ADDR SIZE | download ADDR ALT FILE | upload ADDR SIZE ALT |
//                             command HEX | leave | origin ALT   (ADDR in hex, SIZE in decimal), and the
//                             STM32WB55 ones: ob-read | boot normal|dfu | ob-file FILE | version | otp LEN |
//                             fus-state | fus-delete | fus-upgrade | fus-start
//   dfu_oracle obfile PATH... one line per option bytes file: "bad", or each field the file names, sorted
//   dfu_oracle obdata RAW_HEX NAME=HEX...  the fields read from 128 raw bytes, then data() after setting each
#include <QBuffer>
#include <QCoreApplication>
#include <QCryptographicHash>
#include <QFile>
#include <QLoggingCategory>
#include <QTextStream>

#include <algorithm>
#include <cstdio>
#include <limits>
#include <iostream>
#include <string>

#include "dfusefile.h"
#include "dfumemorylayout.h"
#include "dfusedevice.h"
#include "device/stm32wb55/stm32wb55.h"

using namespace STM32;
using namespace STM32::WB55;

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

    STM32WB55 dev((USBDeviceInfo()));
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
    } else if(op == "ob-read") {
        const auto ob = dev.optionBytes();
        ok = ob.isValid();
        out = ok ? ob.data() : QByteArray();
    } else if(op == "boot") {
        // as Recovery::setBootMode (backend/flipperzero/recovery.cpp)
        const auto normal = arg(3) == "normal";
        auto ob = dev.optionBytes();
        ok = ob.isValid();
        if(ok) {
            ob.setValue("nBOOT0", normal);
            ob.setValue("nSWBOOT0", normal);
            ok = dev.setOptionBytes(ob);
        }
    } else if(op == "ob-file") {
        // as Recovery::downloadOptionBytes
        QFile f(QString::fromLocal8Bit(arg(3)));
        ok = f.open(QIODevice::ReadOnly);
        const OptionBytes loaded(&f);
        ok = ok && loaded.isValid();
        if(ok) {
            const auto actual = dev.optionBytes();
            const auto diff = actual.compare(loaded);
            ok = actual.isValid() && (diff.isEmpty() ? dev.leave() : dev.setOptionBytes(actual.corrected(diff)));
            out = diff.isEmpty() ? "left" : "written";
        }
    } else if(op == "version") {
        const auto v = dev.versionInfo();
        ok = !v.FUSVersion.isEmpty();
        out = (v.FUSVersion + ' ' + v.WirelessVersion).toUtf8();
    } else if(op == "otp") {
        out = dev.OTPData(arg(3).toLongLong());
        ok = !out.isEmpty();
    } else if(op == "fus-state") {
        const auto st = dev.FUSGetState();
        ok = st.isValid();
        out = QByteArray::number((uint)st.status(), 16) + '/' + QByteArray::number((uint)st.error(), 16);
    } else if(op == "radio") {
        // as Recovery::downloadWirelessStack, with the address worked out from SFSA
        QFile f(QString::fromLocal8Bit(arg(3)));
        ok = f.open(QIODevice::ReadOnly) && f.bytesAvailable() > 0;
        const auto ob = ok ? dev.optionBytes() : OptionBytes::invalid();
        ok = ok && ob.isValid();
        if(ok) {
            const auto origin = dev.partitionOrigin((uint8_t)STM32WB55::Partition::Flash);
            const auto pageSize = (quint64)0x1000;
            const auto top = (quint64)origin + pageSize * ob.value("SFSA");
            const auto size = (quint64)f.bytesAvailable();
            ok = !(size > top || ((top - size) & ~(pageSize - 1)) < origin || top > (Q_UINT64_C(1) << 32));
            if(ok) {
                const auto addr = (uint32_t)((top - size) & ~(pageSize - 1));
                out = QByteArray::number(addr, 16);
                ok = dev.erase(addr, f.bytesAvailable()) && dev.download(&f, addr, 0);
            }
        }
    } else if(op == "start-fus") {
        // as Recovery::startFUS
        auto state = dev.FUSGetState();
        ok = state.isValid();
        if(!ok) {
        } else if((state.status() == FUSState::Idle) && (state.error() == FUSState::NoError)) {
            ok = dev.leave();
        } else if((state.status() == FUSState::ErrorOccured) && (state.error() == FUSState::NotRunning)) {
            state = dev.FUSGetState();
        } else {
            ok = false;
        }
    } else if(op == "fus-delete") {
        ok = dev.FUSFwDelete();
    } else if(op == "fus-upgrade") {
        ok = dev.FUSFwUpgrade();
    } else if(op == "fus-start") {
        ok = dev.FUSStartWirelessStack();
    } else {
        return 2;
    }

    dev.endTransaction();
    std::cout << "RESULT " << (ok ? "ok " : "fail ") << hex(out).toStdString() << std::endl;
    return 0;
}

static QByteArray fieldList(const OptionBytes &ob, const QList<QByteArray> &names)
{
    QByteArray out;
    for(const auto &name : names) {
        out += name + '=' + QByteArray::number(ob.value(name), 16) + ' ';
    }
    return out;
}

static int obFileMode(int argc, char *argv[])
{
    QTextStream out(stdout);
    for(int i = 2; i < argc; ++i) {
        QFile f(QString::fromLocal8Bit(argv[i]));
        if(!f.open(QIODevice::ReadOnly)) {
            return 2;
        }
        const OptionBytes ob(&f);
        if(!ob.isValid()) {
            out << "bad\n";
            continue;
        }
        // the fields the file names (value() of any other one fails)
        QList<QByteArray> names;
        for(const auto &name : OptionBytes::fieldNames()) {
            if(name != "IWGDSTDBY" && ob.value(name) != std::numeric_limits<uint32_t>::max()) {
                names.append(name);
            }
        }
        std::sort(names.begin(), names.end());
        out << "ok " << fieldList(ob, names) << '\n';
    }
    return 0;
}

static int obDataMode(int argc, char *argv[])
{
    OptionBytes ob(QByteArray::fromHex(argc > 2 ? argv[2] : ""));
    QTextStream out(stdout);
    if(!ob.isValid()) {
        out << "bad\n";
        return 0;
    }
    QList<QByteArray> names;
    for(const auto &name : OptionBytes::fieldNames()) {
        if(name != "IWGDSTDBY") {
            names.append(name);
        }
    }
    out << fieldList(ob, names) << '\n';
    for(int i = 3; i < argc; ++i) {
        const auto kv = QByteArray(argv[i]).split('=');
        out << (ob.setValue(kv.value(0), kv.value(1).toUInt(nullptr, 16)) ? '1' : '0');
    }
    out << '\n' << ob.data().toHex() << '\n';
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
    } else if(mode == "obfile") {
        return obFileMode(argc, argv);
    } else if(mode == "obdata") {
        return obDataMode(argc, argv);
    }
    std::fprintf(stderr, "usage: dfu_oracle file PATH... | layout | device OP ...\n");
    return 2;
}
