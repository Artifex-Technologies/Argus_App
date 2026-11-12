// The C++ app's PcapExporter (backend/chymaera/exporters/pcapexporter.cpp), for tests/pcap.rs:
//   pcap_oracle OUT pcap|pcapng LINKTYPE < lines of "EPOCH_MS HEX"   (HEX "-" is an empty packet)
// writes OUT with one packet per line. QtCore only; built by build.sh.
#include <QCoreApplication>
#include <QDateTime>
#include <QTimeZone>
#include <iostream>
#include <sstream>
#include <string>

#include "pcapexporter.h"

using namespace Chymaera;

int main(int argc, char *argv[])
{
    QCoreApplication app(argc, argv);
    if(argc != 4) {
        return 2;
    }
    PcapExporter ex;
    const auto format = QByteArray(argv[2]) == "pcap" ? PcapExporter::Pcap : PcapExporter::PcapNg;
    if(!ex.open(QString::fromLocal8Bit(argv[1]), format, static_cast<quint16>(QByteArray(argv[3]).toUInt()))) {
        return 1;
    }
    std::string line;
    while(std::getline(std::cin, line)) {
        std::istringstream in(line);
        long long ms = 0;
        std::string hex;
        in >> ms >> hex;
        const QByteArray data = hex == "-" ? QByteArray() : QByteArray::fromHex(QByteArray::fromStdString(hex));
        if(!ex.writePacket(data, QDateTime::fromMSecsSinceEpoch(ms, QTimeZone::UTC))) {
            return 1;
        }
    }
    ex.close();
    return 0;
}
