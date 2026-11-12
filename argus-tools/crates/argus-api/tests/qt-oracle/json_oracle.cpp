// Qt's own JSON handling, as the C++ Sarina server (backend/chymaera/sarina/sarinaserver.cpp) uses it,
// for the differential test of the Rust port (tests/qt_json.rs). QtCore only; built by build.sh.
//
// stdin: one request line per input line, hex-encoded (the bytes between two '\n' on the socket).
// stdout: one line per input line:
//   skip                         the trimmed line is empty (the server ignores it)
//   err HEX                      the server's error response, compact JSON
//   obj COMPACT INDENTED CMD TOKEN LIMIT ECHO
//                                the parsed request re-serialised compact and indented, the "cmd" and
//                                "token" fields as the server reads them (QJsonValue::toString, then
//                                UTF-8), "limit" as QJsonValue::toInt(50), and {"ok":true} with the
//                                request's "id" echoed as the server does; every field hex
// With --probe it prints a few facts about Qt's QVariant to JSON conversions instead.
#include <QCoreApplication>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QTextStream>
#include <QVariantMap>
#include <cstdio>
#include <iostream>
#include <string>

static QByteArray hex(const QByteArray &b)
{
    return b.isEmpty() ? QByteArray("-") : b.toHex();
}

int main(int argc, char *argv[])
{
    QCoreApplication app(argc, argv);
    QTextStream out(stdout);

    if(argc > 1 && QByteArray(argv[1]) == "--probe") {
        QVariantMap m;
        m[QStringLiteral("nullString")] = QVariant(QMetaType::fromType<QString>());
        m[QStringLiteral("emptyString")] = QString();
        m[QStringLiteral("longlong")] = QVariant(qlonglong(5));
        m[QStringLiteral("double3")] = QVariant(3.0);
        m[QStringLiteral("double35")] = QVariant(3.5);
        m[QStringLiteral("invalid")] = QVariant();
        out << QJsonDocument(QJsonObject::fromVariantMap(m)).toJson(QJsonDocument::Compact) << '\n';
        QJsonObject o { {QStringLiteral("sessionId"), static_cast<double>(7)} };
        o.insert(QStringLiteral("undef"), QJsonValue(QJsonValue::Undefined));
        out << QJsonDocument(o).toJson(QJsonDocument::Compact) << '\n';
        const QString lone = QString(QChar(0xd800)) + QStringLiteral("x");
        out << hex(lone.toUtf8()) << '\n';
        return 0;
    }

    std::string line;
    while(std::getline(std::cin, line)) {
        const QByteArray raw = QByteArray::fromHex(QByteArray::fromStdString(line));
        const QByteArray trimmed = raw.trimmed();
        if(trimmed.isEmpty()) {
            out << "skip\n";
            continue;
        }
        QJsonParseError parseError {};
        const QJsonDocument doc = QJsonDocument::fromJson(trimmed, &parseError);
        if(parseError.error != QJsonParseError::NoError || !doc.isObject()) {
            const QJsonObject e {
                {QStringLiteral("ok"), false},
                {QStringLiteral("error"), QStringLiteral("invalid JSON: %1").arg(parseError.errorString())}
            };
            out << "err " << hex(QJsonDocument(e).toJson(QJsonDocument::Compact)) << '\n';
            continue;
        }
        const QJsonObject request = doc.object();
        QJsonObject response { {QStringLiteral("ok"), true} };
        if(request.contains(QStringLiteral("id"))) {
            response.insert(QStringLiteral("id"), request.value(QStringLiteral("id")));
        }
        out << "obj " << hex(doc.toJson(QJsonDocument::Compact)) << ' ' << hex(doc.toJson(QJsonDocument::Indented)) << ' '
            << hex(request.value(QStringLiteral("cmd")).toString().toUtf8()) << ' '
            << hex(request.value(QStringLiteral("token")).toString().toUtf8()) << ' '
            << request.value(QStringLiteral("limit")).toInt(50) << ' '
            << hex(QJsonDocument(response).toJson(QJsonDocument::Compact)) << '\n';
    }
    return 0;
}
