// The C++ app's capture metadata, as Bridge::recordCapture computes it, for differential tests
// of the Rust port (tests/qt_oracle.rs): for each file named on the command line, prints
// "<kind>\t<meta JSON>" on one line. Built from the app's own parser
// (backend/chymaera/exporters/flipperfileformat.cpp) by build.sh; QtCore only.
#include <QCoreApplication>
#include <QFileInfo>
#include <QJsonDocument>
#include <QJsonObject>
#include <QTextStream>

#include "flipperfileformat.h"

using namespace Chymaera;

// Copied from backend/chymaera/chymaerabridge.cpp (a static function there).
static QString rawKindForSuffix(const QString &suffix)
{
    if(suffix == QStringLiteral("pcap") || suffix == QStringLiteral("pcapng")) {
        return QStringLiteral("pcap");
    } else if(suffix == QStringLiteral("csv")) {
        return QStringLiteral("csv");
    } else if(suffix == QStringLiteral("log") || suffix == QStringLiteral("txt")) {
        return QStringLiteral("log");
    } else {
        return QStringLiteral("file");
    }
}

int main(int argc, char *argv[])
{
    QCoreApplication app(argc, argv);
    QTextStream out(stdout);
    for(int i = 1; i < argc; ++i) {
        const QString path = QString::fromLocal8Bit(argv[i]);
        // as Bridge::recordCapture
        QVariantMap info = FlipperFiles::parseAuto(path);
        if(info.isEmpty()) {
            info[QStringLiteral("kind")] = rawKindForSuffix(QFileInfo(path).suffix().toLower());
        }
        const QByteArray meta = QJsonDocument(QJsonObject::fromVariantMap(info)).toJson(QJsonDocument::Compact);
        out << info.value(QStringLiteral("kind")).toString() << '\t' << QString::fromUtf8(meta) << '\n';
    }
    return 0;
}
