#include "chymaeracli.h"

#include <QDir>
#include <QFile>
#include <QTimer>
#include <QFileInfo>
#include <QEventLoop>
#include <QTextStream>
#include <QTcpSocket>
#include <QHostAddress>
#include <QJsonDocument>
#include <QJsonObject>
#include <QElapsedTimer>
#include <QCoreApplication>

#include "chymaera/chymaerabridge.h"
#include "chymaera/datastore/chymaeradatastore.h"
#include "chymaera/eventlog/chymaeraeventlog.h"
#include "chymaera/exporters/pcapexporter.h"
#include "chymaera/exporters/flipperfileformat.h"
#include "chymaera/sarina/sarinaserver.h"

#include "applicationbackend.h"
#include "flipperzero/flipperzero.h"

using namespace Chymaera;

namespace {

QTextStream &out()
{
    static QTextStream s(stdout);
    return s;
}

QString tempFile(const QString &name)
{
    return QDir(QDir::tempPath()).filePath(name);
}

bool writeTextFile(const QString &path, const QByteArray &content)
{
    QFile f(path);
    if(!f.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
        return false;
    }
    f.write(content);
    return true;
}

// One self-test check. Prints a PASS/FAIL line and accumulates failures.
bool check(const QString &name, bool ok, const QString &detail, int &failures)
{
    out() << (ok ? "  PASS  " : "  FAIL  ") << name;
    if(!ok && !detail.isEmpty()) {
        out() << "  — " << detail;
    }
    out() << "\n";
    out().flush();
    if(!ok) {
        ++failures;
    }
    return ok;
}

} // namespace

int ChymaeraCli::run(QCoreApplication &app, const QStringList &sub)
{
    const QString cmd = sub.isEmpty() ? QString() : sub.first();
    const QStringList rest = sub.mid(1);

    if(cmd == QStringLiteral("selftest")) {
        return cmdSelfTest();
    } else if(cmd == QStringLiteral("serve")) {
        return cmdServe(app, rest);
    } else if(cmd == QStringLiteral("import")) {
        return cmdImport(rest);
    } else if(cmd == QStringLiteral("parse")) {
        return cmdParse(rest);
    } else if(cmd == QStringLiteral("watch")) {
        return cmdWatch(app, rest);
    } else if(cmd == QStringLiteral("pull")) {
        return cmdPull(app, rest);
    }

    printUsage();
    return cmd.isEmpty() ? 0 : 1;
}

void ChymaeraCli::printUsage()
{
    out() << "Chymaera headless interface\n\n"
          << "Usage: qFlipper-cli chymaera <command> [args]\n\n"
          << "Commands:\n"
          << "  selftest              Verify the Chymaera subsystem end-to-end (no device needed)\n"
          << "  serve [port] [token]  Start the Sarina control API (loopback). Default port 44700\n"
          << "  parse <file>          Parse a Flipper file (.nfc/.sub/...) and print JSON\n"
          << "  import <file>         Parse and record a capture in the datastore\n"
          << "  watch [seconds]       Stream live device events from a connected Flipper\n"
          << "  pull <remotePath>     Pull a device directory (e.g. /ext/nfc) into the datastore\n";
    out().flush();
}

int ChymaeraCli::cmdSelfTest()
{
    int failures = 0;
    out() << "Chymaera self-test\n";
    out().flush();

    // --- datastore ---
    {
        const QString dbPath = tempFile(QStringLiteral("chymaera_selftest.db"));
        QFile::remove(dbPath);

        ChymaeraDatastore ds;
        bool ok = ds.open(dbPath);
        check(QStringLiteral("datastore open"), ok, ds.lastError(), failures);

        if(ok) {
            const qint64 sid = ds.startSession(QStringLiteral("selftest"));
            check(QStringLiteral("datastore session"), sid >= 0, ds.lastError(), failures);

            ds.logEvent(Severity::Info, Source::System, QStringLiteral("hello"));
            check(QStringLiteral("datastore event"), ds.eventCount() >= 1, QString(), failures);

            ds.addCapture(QStringLiteral("nfc"), QStringLiteral("x.nfc"),
                          QStringLiteral("/tmp/x.nfc"), 10, QStringLiteral("{}"));
            check(QStringLiteral("datastore capture"), !ds.captures(10).isEmpty(), QString(), failures);

            ds.endSession();
            ds.close();
        }
        QFile::remove(dbPath);
        QFile::remove(dbPath + QStringLiteral("-wal"));
        QFile::remove(dbPath + QStringLiteral("-shm"));
    }

    // --- pcap / pcapng exporter ---
    {
        const QString pcapng = tempFile(QStringLiteral("chymaera_selftest.pcapng"));
        PcapExporter ex;
        bool ok = ex.open(pcapng, PcapExporter::PcapNg);
        if(ok) {
            ex.writePacket(QByteArray::fromHex("deadbeef"));
            ex.writePacket(QByteArray(5, '\0'));
            ex.close();
        }
        check(QStringLiteral("pcapng write"), ok && QFileInfo(pcapng).size() > 60, ex.lastError(), failures);
        QFile::remove(pcapng);

        const QString pcap = tempFile(QStringLiteral("chymaera_selftest.pcap"));
        QString err;
        const bool wrote = PcapExporter::writeFile(pcap, { QByteArray::fromHex("0102030405") },
                                                   PcapExporter::Pcap, PcapExporter::LinkUser0, &err);
        check(QStringLiteral("pcap write"), wrote && QFileInfo(pcap).size() >= (24 + 16 + 5), err, failures);
        QFile::remove(pcap);
    }

    // --- Flipper file parsers ---
    {
        const QString sub = tempFile(QStringLiteral("chymaera_selftest.sub"));
        writeTextFile(sub,
            "Filetype: Flipper SubGhz RAW File\n"
            "Version: 1\n"
            "Frequency: 433920000\n"
            "Preset: FuriHalSubGhzPresetOok650Async\n"
            "Protocol: RAW\n"
            "RAW_Data: 100 -200 300 -400 500\n");
        const QVariantMap sm = FlipperFiles::parseSubGhz(sub);
        check(QStringLiteral("parse .sub frequency"),
              sm.value(QStringLiteral("frequency")).toString() == QStringLiteral("433920000"),
              QString(), failures);
        check(QStringLiteral("parse .sub raw count"),
              sm.value(QStringLiteral("rawSampleCount")).toInt() == 5, QString(), failures);
        QFile::remove(sub);

        const QString nfc = tempFile(QStringLiteral("chymaera_selftest.nfc"));
        writeTextFile(nfc,
            "Filetype: Flipper NFC device\n"
            "Version: 2\n"
            "Device type: UID\n"
            "UID: 04 A2 26 BA\n"
            "ATQA: 00 44\n"
            "SAK: 00\n");
        const QVariantMap nm = FlipperFiles::parseNfc(nfc);
        check(QStringLiteral("parse .nfc uid"),
              nm.value(QStringLiteral("uid")).toString() == QStringLiteral("04 A2 26 BA"),
              QString(), failures);
        QFile::remove(nfc);
    }

    // --- Sarina control API round-trip (ping -> pong) ---
    {
        Bridge bridge;
        bool started = bridge.startServer(0); // ephemeral port
        check(QStringLiteral("control API listen"), started, QString(), failures);

        if(started) {
            const quint16 port = static_cast<quint16>(bridge.serverPort());
            QTcpSocket sock;
            sock.connectToHost(QHostAddress::LocalHost, port);

            QByteArray buffer;
            QElapsedTimer timer;
            timer.start();
            bool sent = false;
            while(timer.elapsed() < 3000) {
                QCoreApplication::processEvents(QEventLoop::AllEvents, 50);
                if(!sent && sock.state() == QAbstractSocket::ConnectedState) {
                    sock.write("{\"cmd\":\"ping\"}\n");
                    sock.flush();
                    sent = true;
                }
                buffer.append(sock.readAll());
                if(buffer.contains('\n')) {
                    break;
                }
            }

            const QJsonObject reply = QJsonDocument::fromJson(buffer.trimmed()).object();
            check(QStringLiteral("control API ping"),
                  reply.value(QStringLiteral("ok")).toBool() && reply.value(QStringLiteral("pong")).toBool(),
                  QString::fromUtf8(buffer.trimmed()), failures);

            bridge.stopServer();
        }
    }

    out() << "\n" << (failures == 0 ? "SELF-TEST PASSED" : QStringLiteral("SELF-TEST FAILED (%1)").arg(failures)) << "\n";
    out().flush();
    return failures == 0 ? 0 : 1;
}

int ChymaeraCli::cmdServe(QCoreApplication &app, const QStringList &args)
{
    int port = SarinaServer::DefaultPort;
    QString token;

    if(!args.isEmpty()) {
        bool ok = false;
        const int p = args.first().toInt(&ok);
        if(ok) {
            port = p;
        }
    }
    if(args.size() > 1) {
        token = args.at(1);
    }

    Bridge bridge;

    // Mirror the live event stream to stdout so the operator can watch it.
    QObject::connect(bridge.eventLog(), &ChymaeraEventLog::entryAppended,
                     [](int, const QString &source, const QString &message) {
        out() << "[" << source << "] " << message << "\n";
        out().flush();
    });

    if(!bridge.startServer(port, token)) {
        out() << "Failed to start the Sarina control API on port " << port << "\n";
        out().flush();
        return 1;
    }

    out() << "Sarina control API listening on 127.0.0.1:" << bridge.serverPort()
          << (token.isEmpty() ? " (no token)" : " (token required)") << "\n"
          << "Datastore: " << bridge.datastorePath() << "\n"
          << "Press Ctrl+C to stop.\n";
    out().flush();

    return app.exec();
}

int ChymaeraCli::cmdParse(const QStringList &args)
{
    if(args.isEmpty()) {
        out() << "Usage: qFlipper-cli chymaera parse <file>\n";
        out().flush();
        return 1;
    }

    const QVariantMap info = FlipperFiles::parseAuto(args.first());
    if(info.isEmpty()) {
        out() << "Could not parse: " << args.first() << "\n";
        out().flush();
        return 1;
    }

    out() << QString::fromUtf8(QJsonDocument(QJsonObject::fromVariantMap(info)).toJson(QJsonDocument::Indented));
    out().flush();
    return 0;
}

int ChymaeraCli::cmdImport(const QStringList &args)
{
    if(args.isEmpty()) {
        out() << "Usage: qFlipper-cli chymaera import <file>\n";
        out().flush();
        return 1;
    }

    Bridge bridge;
    const QString summary = bridge.importCapture(args.first());
    if(summary.isEmpty()) {
        out() << "Import failed: " << args.first() << "\n";
        out().flush();
        return 1;
    }

    out() << "Imported: " << summary << "\n"
          << "Datastore: " << bridge.datastorePath() << "\n";
    out().flush();
    return 0;
}

int ChymaeraCli::cmdWatch(QCoreApplication &app, const QStringList &args)
{
    int seconds = 0; // 0 == run until Ctrl+C
    if(!args.isEmpty()) {
        bool ok = false;
        const int s = args.first().toInt(&ok);
        if(ok) {
            seconds = s;
        }
    }

    ApplicationBackend backend;
    Bridge bridge;

    QObject::connect(bridge.eventLog(), &ChymaeraEventLog::entryAppended,
                     [](int, const QString &source, const QString &message) {
        out() << "[" << source << "] " << message << "\n";
        out().flush();
    });

    QObject::connect(&backend, &ApplicationBackend::currentDeviceChanged, &app, [&backend, &bridge]() {
        bridge.attachDevice(backend.device());
    });

    out() << "Watching for device events" << (seconds > 0 ? QStringLiteral(" for %1s").arg(seconds) : QString())
          << " (Ctrl+C to stop)…\n";
    out().flush();

    if(seconds > 0) {
        QTimer::singleShot(seconds * 1000, &app, [&app]() { app.quit(); });
    }

    return app.exec();
}

int ChymaeraCli::cmdPull(QCoreApplication &app, const QStringList &args)
{
    if(args.isEmpty()) {
        out() << "Usage: qFlipper-cli chymaera pull <remotePath>   (e.g. /ext/nfc)\n";
        out().flush();
        return 1;
    }

    const QString remote = args.first();

    ApplicationBackend backend;
    Bridge bridge;
    int rc = 2; // stays 2 only if we time out before finishing
    bool triggered = false;

    QObject::connect(bridge.eventLog(), &ChymaeraEventLog::entryAppended,
                     [](int, const QString &source, const QString &message) {
        out() << "[" << source << "] " << message << "\n";
        out().flush();
    });

    QObject::connect(&bridge, &Bridge::pullFinished, &app, [&app, &rc](bool ok, int count) {
        rc = ok ? 0 : 1;
        out() << (ok ? "Pull complete: " : "Pull failed after ") << count << " file(s)\n";
        out().flush();
        app.quit();
    });

    QObject::connect(&backend, &ApplicationBackend::backendStateChanged, &app,
                     [&app, &backend, &bridge, &rc, &triggered, remote]() {
        const auto state = backend.backendState();
        if(!triggered && state == ApplicationBackend::BackendState::Ready && backend.device()) {
            triggered = true;
            bridge.attachDevice(backend.device());
            if(!bridge.pullPath(remote)) {
                rc = 1;
                app.quit();
            }
        } else if(state == ApplicationBackend::BackendState::ErrorOccured) {
            out() << "Device error while preparing pull.\n";
            out().flush();
            rc = 1;
            app.quit();
        }
    });

    out() << "Waiting for device to become ready, then pulling " << remote << "…\n";
    out().flush();

    // Hard safety timeout so the command never hangs forever headlessly.
    QTimer::singleShot(300000, &app, [&app, &triggered]() {
        if(!triggered) {
            out() << "Timed out waiting for a ready device.\n";
            out().flush();
        }
        app.quit();
    });

    app.exec();
    return rc;
}
