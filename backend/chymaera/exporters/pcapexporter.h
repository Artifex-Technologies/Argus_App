#pragma once

#include <QFile>
#include <QString>
#include <QDateTime>
#include <QByteArray>

namespace Chymaera {

/* Writes packet captures Wireshark can open (Roadmap Phase 3 — Wireshark
 * bridge). Supports both the classic libpcap format (.pcap) and the modern
 * pcapng (.pcapng). Pure QtCore; the byte layout is written by hand so the
 * output is exact and endianness is explicit (little-endian on disk).
 *
 * Link-layer type defaults to LINKTYPE_USER0 (147), the DLT reserved for
 * private/experimental use — appropriate for opaque Flipper payloads (Sub-GHz
 * frames, NFC exchanges) that have no standard DLT. Pass a real DLT (e.g.
 * LINKTYPE_ETHERNET, LINKTYPE_BLUETOOTH_LE_LL) when the data warrants it. */

class PcapExporter
{
public:
    enum Format {
        Pcap,   // classic libpcap
        PcapNg  // pcapng
    };

    // A few common DLT / LINKTYPE values (see www.tcpdump.org/linktypes.html).
    enum LinkType : quint16 {
        LinkNull        = 0,
        LinkEthernet    = 1,
        LinkUser0       = 147, // private use — default for opaque payloads
        LinkBluetoothLE = 251, // LINKTYPE_BLUETOOTH_LE_LL
        LinkNordicBLE   = 272  // LINKTYPE_NORDIC_BLE
    };

    PcapExporter();
    ~PcapExporter();

    // Open a capture file for writing. Returns false on I/O error.
    bool open(const QString &path, Format format = PcapNg, quint16 linkType = LinkUser0);

    // Append one packet. Timestamp defaults to now; snaplen is honoured.
    bool writePacket(const QByteArray &packet, const QDateTime &timestamp = QDateTime::currentDateTimeUtc());

    void close();

    bool isOpen() const;
    int packetsWritten() const;
    QString lastError() const;
    QString path() const;

    // Convenience: write a whole batch to a fresh file in one call.
    static bool writeFile(const QString &path, const QList<QByteArray> &packets,
                          Format format = PcapNg, quint16 linkType = LinkUser0,
                          QString *errorOut = nullptr);

private:
    void writeU16(quint16 v);
    void writeU32(quint32 v);
    void writeU64(quint64 v);
    void writeBytes(const QByteArray &b);
    void writePcapGlobalHeader();
    void writePcapNgHeaders();

    QFile m_file;
    Format m_format;
    quint16 m_linkType;
    quint32 m_snaplen;
    int m_packetsWritten;
    QString m_lastError;
};

}
