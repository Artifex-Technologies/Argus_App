// The C++ app's own update directory and region code, for tests/qt_oracle.rs (built by build.sh):
//   updates_oracle dir FILE...              UpdateRegistry::fillFromJson on each file
//   updates_oracle region FALLBACK FILE...  RegionInfo and RegionData as RegionProvisioningOperation uses them
//   updates_oracle fetch NAME SHA256 MAX    RemoteFileFetcher (its test hook: ARGUS_TEST_FIRMWARE_DIR) on one file
// One block of lines per file, in the format tests/qt_oracle.rs prints for the Rust port. Opens no device and
// no network (the fetch goes through the test hook, which reads a local folder).
#include <QCoreApplication>
#include <QBuffer>
#include <QFile>
#include <QTextStream>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLoggingCategory>

#include "updateregistry.h"
#include "flipperupdates.h"
#include "regioninfo.h"
#include "regiondata.h"
#include "remotefilefetcher.h"

Q_LOGGING_CATEGORY(CATEGORY_DEBUG, "DBG")

using namespace Flipper;

static QTextStream out(stdout);

// UTF-16 code units: printable ASCII but space, backslash and comma as they are, the rest as \uXXXX.
static QString esc(const QString &s)
{
    QString r;
    for(const auto c : s) {
        const auto u = c.unicode();
        if(u > 0x20 && u < 0x7f && u != '\\' && u != ',') {
            r += c;
        } else {
            r += QStringLiteral("\\u%1").arg(uint(u), 4, 16, QLatin1Char('0'));
        }
    }
    return r.isEmpty() ? QStringLiteral("-") : r;
}

class OracleRegistry : public UpdateRegistry
{
public:
    OracleRegistry(): UpdateRegistry(QString()) {}
private:
    const QString updateChannel() const override { return QStringLiteral("release"); }
};

static void directory(const QByteArray &text)
{
    OracleRegistry reg;
    reg.fillFromJson(text);
    const auto names = reg.channelNames();
    if(names.isEmpty()) {
        out << "invalid\n";
        return;
    }
    QStringList escaped;
    for(const auto &n : names) escaped << esc(n);
    out << "names " << escaped.join(',') << "\n";
    for(const auto &n : names) {
        const auto ch = reg.channel(n);
        out << "channel " << esc(ch.name()) << " " << esc(ch.title()) << " " << esc(ch.description())
            << " versions " << ch.versions().size() << " latest " << esc(ch.latestVersion().number()) << "\n";
        for(const auto &v : ch.versions()) {
            out << "version " << esc(v.number()) << " " << esc(v.changelog()) << " files " << v.files().size() << "\n";
            for(const auto &f : v.files()) {
                out << "file " << esc(f.type()) << " " << esc(f.target()) << " " << esc(f.url()) << " " << f.sha256() << "\n";
            }
        }
    }
    // a channel the directory does not have: no latest version (this read past an empty list)
    out << "missing " << esc(reg.channel(QStringLiteral("no-such-channel")).latestVersion().number()) << "\n";
}

static void region(const QByteArray &fallback, const QByteArray &text)
{
    const RegionInfo info(text);
    if(!info.isValid()) {
        out << "invalid\n";
        return;
    } else if(info.isError()) {
        out << "error " << info.errorCode() << " " << esc(info.errorString()) << "\n";
        return;
    }
    const auto country = info.countryOr(fallback);
    const auto keys = info.countryBandKeys(country);
    QStringList hexKeys;
    for(const auto &k : keys) hexKeys << QString::fromLatin1(k.toHex());
    BandInfoList bands;
    for(const auto &b : info.bandsByKeys(keys)) {
        bands.append({b.start, b.end, b.powerLimit, b.dutyCycle});
    }
    const auto data = RegionData(country, bands).encode();
    out << "ok " << (country.isEmpty() ? QByteArray("-") : country.toHex()) << " " << (hexKeys.isEmpty() ? QStringLiteral("-") : hexKeys.join(','))
        << " " << data.toHex() << "\n";
}

int main(int argc, char **argv)
{
    QCoreApplication app(argc, argv);
    const auto args = app.arguments();
    if(args.size() >= 3 && args[1] == QStringLiteral("dir")) {
        for(int i = 2; i < args.size(); ++i) {
            QFile f(args[i]);
            if(!f.open(QIODevice::ReadOnly)) return 2;
            directory(f.readAll());
        }
    } else if(args.size() >= 4 && args[1] == QStringLiteral("region")) {
        const auto fallback = args[2] == QStringLiteral("-") ? QByteArray() : args[2].toUtf8();
        for(int i = 3; i < args.size(); ++i) {
            QFile f(args[i]);
            if(!f.open(QIODevice::ReadOnly)) return 2;
            region(fallback, f.readAll());
        }
    } else if(args.size() == 5 && args[1] == QStringLiteral("fetch")) {
        const auto json = QStringLiteral("{\"target\":\"f7\",\"type\":\"t\",\"url\":\"https://update.invalid/firmware/%1\",\"sha256\":\"%2\"}")
                .arg(args[2], args[3]);
        Updates::FileInfo info;
        try {
            info = Updates::FileInfo(QJsonDocument::fromJson(json.toUtf8()).object());
        } catch(std::runtime_error &) {
            out << "invalid\n";
            return 0;
        }
        QBuffer buf;
        RemoteFileFetcher fetcher;
        fetcher.setMaxSize(args[4].toLongLong());
        QObject::connect(&fetcher, &RemoteFileFetcher::finished, &app, &QCoreApplication::quit);
        if(!fetcher.fetch(info, &buf)) {
            out << "failed\n";
            return 0;
        }
        app.exec();
        out << (fetcher.isError() ? QStringLiteral("failed") : QStringLiteral("ok %1").arg(buf.data().size())) << "\n";
    } else {
        return 2;
    }
    return 0;
}
