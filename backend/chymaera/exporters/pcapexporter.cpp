#include "pcapexporter.h"

using namespace Chymaera;

namespace {
constexpr quint32 kSnaplen = 262144;

// pcapng block type codes
constexpr quint32 kBlockSHB = 0x0A0D0D0A; // Section Header Block
constexpr quint32 kBlockIDB = 0x00000001; // Interface Description Block
constexpr quint32 kBlockEPB = 0x00000006; // Enhanced Packet Block
constexpr quint32 kByteOrderMagic = 0x1A2B3C4D;

// classic pcap magic for microsecond-resolution timestamps
constexpr quint32 kPcapMagicUsec = 0xA1B2C3D4;
}

PcapExporter::PcapExporter():
    m_format(PcapNg),
    m_linkType(LinkUser0),
    m_snaplen(kSnaplen),
    m_packetsWritten(0)
{}

PcapExporter::~PcapExporter()
{
    close();
}

bool PcapExporter::open(const QString &path, Format format, quint16 linkType)
{
    close();

    m_format = format;
    m_linkType = linkType;
    m_snaplen = kSnaplen;
    m_packetsWritten = 0;

    m_file.setFileName(path);
    if(!m_file.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
        m_lastError = QStringLiteral("Cannot open %1: %2").arg(path, m_file.errorString());
        return false;
    }

    if(m_format == Pcap) {
        writePcapGlobalHeader();
    } else {
        writePcapNgHeaders();
    }

    return true;
}

bool PcapExporter::writePacket(const QByteArray &packet, const QDateTime &timestamp)
{
    if(!m_file.isOpen()) {
        m_lastError = QStringLiteral("No capture file is open");
        return false;
    }

    const QByteArray captured = packet.left(static_cast<int>(m_snaplen));
    const quint32 origLen = static_cast<quint32>(packet.size());
    const quint32 capLen = static_cast<quint32>(captured.size());

    const qint64 ms = timestamp.toMSecsSinceEpoch();
    const quint64 sec = static_cast<quint64>(ms / 1000);
    const quint64 usec = static_cast<quint64>((ms % 1000) * 1000);

    if(m_format == Pcap) {
        writeU32(static_cast<quint32>(sec));
        writeU32(static_cast<quint32>(usec));
        writeU32(capLen);
        writeU32(origLen);
        writeBytes(captured);
    } else {
        // Enhanced Packet Block. Packet data is padded to a 32-bit boundary.
        const quint32 pad = (4 - (capLen % 4)) % 4;
        const quint32 blockLen = 32 + capLen + pad; // 4*7 fixed fields + data + pad + trailing len
        // pcapng EPB timestamp is a 64-bit count in units of 10^-6 s (default).
        const quint64 tsUnits = sec * 1000000ull + usec;

        writeU32(kBlockEPB);
        writeU32(blockLen);
        writeU32(0);                                   // interface id
        writeU32(static_cast<quint32>(tsUnits >> 32)); // timestamp high
        writeU32(static_cast<quint32>(tsUnits & 0xFFFFFFFFull)); // timestamp low
        writeU32(capLen);
        writeU32(origLen);
        writeBytes(captured);
        for(quint32 i = 0; i < pad; ++i) {
            m_file.putChar('\0');
        }
        writeU32(blockLen); // trailing block total length
    }

    ++m_packetsWritten;
    return true;
}

void PcapExporter::close()
{
    if(m_file.isOpen()) {
        m_file.flush();
        m_file.close();
    }
}

bool PcapExporter::isOpen() const
{
    return m_file.isOpen();
}

int PcapExporter::packetsWritten() const
{
    return m_packetsWritten;
}

QString PcapExporter::lastError() const
{
    return m_lastError;
}

QString PcapExporter::path() const
{
    return m_file.fileName();
}

bool PcapExporter::writeFile(const QString &path, const QList<QByteArray> &packets,
                             Format format, quint16 linkType, QString *errorOut)
{
    PcapExporter exporter;
    if(!exporter.open(path, format, linkType)) {
        if(errorOut) { *errorOut = exporter.lastError(); }
        return false;
    }

    for(const auto &p : packets) {
        if(!exporter.writePacket(p)) {
            if(errorOut) { *errorOut = exporter.lastError(); }
            exporter.close();
            return false;
        }
    }

    exporter.close();
    return true;
}

void PcapExporter::writeU16(quint16 v)
{
    char b[2] = { char(v & 0xFF), char((v >> 8) & 0xFF) };
    m_file.write(b, 2);
}

void PcapExporter::writeU32(quint32 v)
{
    char b[4] = {
        char(v & 0xFF), char((v >> 8) & 0xFF),
        char((v >> 16) & 0xFF), char((v >> 24) & 0xFF)
    };
    m_file.write(b, 4);
}

void PcapExporter::writeU64(quint64 v)
{
    writeU32(static_cast<quint32>(v & 0xFFFFFFFFull));
    writeU32(static_cast<quint32>(v >> 32));
}

void PcapExporter::writeBytes(const QByteArray &b)
{
    m_file.write(b);
}

void PcapExporter::writePcapGlobalHeader()
{
    writeU32(kPcapMagicUsec);
    writeU16(2);            // version major
    writeU16(4);            // version minor
    writeU32(0);            // thiszone (GMT to local correction)
    writeU32(0);            // sigfigs
    writeU32(m_snaplen);    // snaplen
    writeU32(m_linkType);   // network (data link type)
}

void PcapExporter::writePcapNgHeaders()
{
    // Section Header Block (no options).
    writeU32(kBlockSHB);
    writeU32(28);                 // block total length
    writeU32(kByteOrderMagic);
    writeU16(1);                  // major version
    writeU16(0);                  // minor version
    writeU64(0xFFFFFFFFFFFFFFFFull); // section length: unspecified
    writeU32(28);                 // trailing block total length

    // Interface Description Block (no options).
    writeU32(kBlockIDB);
    writeU32(20);                 // block total length
    writeU16(m_linkType);         // link type
    writeU16(0);                  // reserved
    writeU32(m_snaplen);          // snap length
    writeU32(20);                 // trailing block total length
}
