#include "arguscli.h"

#include <QDir>
#include <QFile>
#include <QTimer>
#include <QFileInfo>
#include <QEventLoop>
#include <QJsonArray>
#include <QTextStream>
#include <QJsonDocument>
#include <QTemporaryDir>
#include <QCoreApplication>
#include <QCryptographicHash>

#include "argus/argustools.h"
#include "argus/devicestorage.h"
#include "argus/arguscontroller.h"
#include "chymaera/chymaerabridge.h"
#include "applicationbackend.h"
#include "flipperzero/flipperzero.h"

using namespace Argus;

namespace {

QTextStream &out()
{
    static QTextStream s(stdout);
    return s;
}

struct Result {
    bool ok = false;
    QString message;
};

/* Call start(), then wait for operationFinished(op) (or time out). */
Result await(ArgusController &c, const QString &op, std::function<void()> start, int timeoutMs = 120000)
{
    Result r;
    QEventLoop loop;
    bool done = false;
    auto conn = QObject::connect(&c, &ArgusController::operationFinished, &loop, [&](const QString &o, bool ok, const QString &msg) {
        if(o == op && !done) {
            done = true;
            r.ok = ok;
            r.message = msg;
            loop.quit();
        }
    });
    QTimer::singleShot(timeoutMs, &loop, [&]() {
        if(!done) {
            r.message = QStringLiteral("timed out");
            loop.quit();
        }
    });
    start();
    if(!done) {
        loop.exec();
    }
    QObject::disconnect(conn);
    // let follow-up work (a refresh started at the end) settle
    QEventLoop settle;
    QTimer::singleShot(50, &settle, &QEventLoop::quit);
    settle.exec();
    return r;
}

bool check(const QString &name, bool ok, const QString &detail, int &failures)
{
    out() << (ok ? "  PASS  " : "  FAIL  ") << name;
    if(!detail.isEmpty()) {
        out() << "  — " << detail;
    }
    out() << "\n";
    out().flush();
    failures += ok ? 0 : 1;
    return ok;
}

void put(const QString &card, const QString &remote, const QByteArray &data)
{
    const auto p = card + remote;
    QDir().mkpath(QFileInfo(p).absolutePath());
    QFile f(p);
    f.open(QIODevice::WriteOnly | QIODevice::Truncate);
    f.write(data);
}

QByteArray slurp(const QString &p)
{
    QFile f(p);
    return f.open(QIODevice::ReadOnly) ? f.readAll() : QByteArray();
}

// A copy of a real FAP built for another API (the manifest follows the "HDGR" magic + version 1).
QByteArray withApi(QByteArray fap, quint16 major, quint16 minor)
{
    const auto at = fap.indexOf(QByteArray("HDGR\x01\x00\x00\x00", 8));
    if(at >= 0) {
        fap[at + 8] = char(minor & 0xff);
        fap[at + 9] = char(minor >> 8);
        fap[at + 10] = char(major & 0xff);
        fap[at + 11] = char(major >> 8);
    }
    return fap;
}

QVariantMap findApp(const QVariantList &apps, const QString &fileName)
{
    for(const auto &a : apps) {
        if(a.toMap().value(QStringLiteral("file_name")).toString() == fileName) {
            return a.toMap();
        }
    }
    return {};
}

} // namespace

int ArgusCli::runTest(QCoreApplication &app, const QStringList &args)
{
    Q_UNUSED(app)
    int failures = 0;
    out() << "Argus features test (fake SD card, no device)\n";

    // everything Argus writes goes to a throwaway XDG data folder
    QTemporaryDir home;
    qputenv("XDG_DATA_HOME", home.path().toUtf8());

    const auto fixtures = args.value(0, QDir(QCoreApplication::applicationDirPath()).filePath(QStringLiteral("../argus-tools/tests/fixtures")));
    if(!check(QStringLiteral("argus-tools binary found"), ArgusTools::isAvailable(), ArgusTools::program(), failures)
       || !check(QStringLiteral("fixtures found"), QFileInfo(QDir(fixtures).filePath(QStringLiteral("update-f7-test.tgz"))).isFile(), fixtures, failures)) {
        return 1;
    }
    const auto pkg = QDir(fixtures).filePath(QStringLiteral("update-f7-test.tgz"));
    const auto snake = slurp(QDir(fixtures).filePath(QStringLiteral("snake_game.fap")));

    const auto card = QDir(home.path()).filePath(QStringLiteral("card"));
    put(card, QStringLiteral("/ext/apps/Games/snake_game.fap"), withApi(snake, 86, 0));
    put(card, QStringLiteral("/ext/subghz/garage.sub"), slurp(QDir(fixtures).filePath(QStringLiteral("captures/ext/subghz/garage.sub"))));
    put(card, QStringLiteral("/ext/nfc/badge.nfc"), slurp(QDir(fixtures).filePath(QStringLiteral("captures/ext/nfc/badge.nfc"))));
    put(card, QStringLiteral("/ext/nfc/assets/mf_classic_dict_user.nfc"), "A0A1A2A3A4A5\n");

    Chymaera::Bridge bridge;
    auto &c = *bridge.argus();
    c.useFakeCard(card, QStringLiteral("88.2/7"));
    QEventLoop settle;
    QTimer::singleShot(50, &settle, &QEventLoop::quit);
    settle.exec();
    check(QStringLiteral("card attached, API read"), c.isCardReady() && c.firmwareApi() == QLatin1String("88.2/7") && c.isFirmwareApiFromDevice(), c.firmwareApi(), failures);

    // 1. extractor
    auto r = await(c, QStringLiteral("package"), [&]() { c.scanPackage(pkg); });
    const auto apps = c.packageReport().value(QStringLiteral("apps")).toList();
    check(QStringLiteral("package scan lists 2 apps"), r.ok && apps.size() == 2, r.message, failures);
    const auto snakeApp = findApp(apps, QStringLiteral("snake_game.fap"));
    check(QStringLiteral("snake is compatible with 88.2, category Games"),
          snakeApp.value(QStringLiteral("compat")).toMap().value(QStringLiteral("status")).toString() == QLatin1String("compatible")
              && snakeApp.value(QStringLiteral("category")).toString() == QLatin1String("Games"), QString(), failures);

    // 2. install: never overwrite without asking
    r = await(c, QStringLiteral("installPlan"), [&]() { c.prepareInstall(apps, false); });
    auto plan = c.installPlan();
    int existing = 0;
    for(const auto &v : plan) {
        existing += v.toMap().value(QStringLiteral("exists")).toBool();
    }
    check(QStringLiteral("install plan finds the one already on the card"), r.ok && plan.size() == 2 && existing == 1, r.message, failures);
    r = await(c, QStringLiteral("install"), [&]() { c.runInstall(plan); });
    check(QStringLiteral("install writes the new app, keeps the existing one"),
          r.ok && slurp(card + QStringLiteral("/ext/apps/Sub-GHz/subghz_jammer_detect.fap")) == slurp(QDir(fixtures).filePath(QStringLiteral("subghz_jammer_detect.fap")))
              && slurp(card + QStringLiteral("/ext/apps/Games/snake_game.fap")) != snake, r.message, failures);

    // refusals
    QVariantMap incompatible = snakeApp;
    incompatible[QStringLiteral("compat")] = QVariantMap { {QStringLiteral("status"), QStringLiteral("incompatible")}, {QStringLiteral("code"), QStringLiteral("api_too_old")} };
    QVariantMap blocked = snakeApp;
    blocked[QStringLiteral("compat")] = QVariantMap { {QStringLiteral("status"), QStringLiteral("incompatible")}, {QStringLiteral("blocked"), true}, {QStringLiteral("reason"), QStringLiteral("oversized")} };
    QVariantMap escape = snakeApp;
    escape[QStringLiteral("category")] = QStringLiteral("../../int");
    r = await(c, QStringLiteral("installPlan"), [&]() { c.prepareInstall({incompatible, escape}, false); });
    plan = c.installPlan();
    check(QStringLiteral("incompatible and path-escaping apps are refused"), plan.size() == 2 && plan[0].toMap().value(QStringLiteral("refused")).toBool()
          && plan[1].toMap().value(QStringLiteral("refused")).toBool(), r.message, failures);
    r = await(c, QStringLiteral("installPlan"), [&]() { c.prepareInstall({incompatible, blocked}, true); });
    plan = c.installPlan();
    check(QStringLiteral("force allows incompatible, never blocked"), plan.size() == 2 && !plan[0].toMap().value(QStringLiteral("refused")).toBool()
          && plan[1].toMap().value(QStringLiteral("refused")).toBool(), r.message, failures);

    // 3. check-up with a package that has a compatible copy
    r = await(c, QStringLiteral("checkup"), [&]() { c.runCheckup({pkg}); });
    const auto checked = c.checkupReport().value(QStringLiteral("apps")).toList();
    QVariantMap broken;
    for(const auto &v : checked) {
        if(v.toMap().value(QStringLiteral("path")).toString() == QLatin1String("/ext/apps/Games/snake_game.fap")) {
            broken = v.toMap();
        }
    }
    check(QStringLiteral("check-up finds the API 86 app and a compatible copy"),
          r.ok && c.checkupReport().value(QStringLiteral("incompatible")).toInt() == 1
              && broken.value(QStringLiteral("compat")).toMap().value(QStringLiteral("code")).toString() == QLatin1String("api_too_old")
              && !broken.value(QStringLiteral("replacement")).toMap().isEmpty(), r.message, failures);
    auto replacement = broken.value(QStringLiteral("replacement")).toMap();
    replacement[QStringLiteral("target")] = broken.value(QStringLiteral("path"));
    r = await(c, QStringLiteral("installPlan"), [&]() { c.prepareInstall({replacement}, false); });
    plan = c.installPlan();
    auto item = plan.value(0).toMap();
    item[QStringLiteral("overwrite")] = true; // the user confirmed
    r = await(c, QStringLiteral("install"), [&]() { c.runInstall({item}); });
    check(QStringLiteral("the compatible copy replaces it after confirmation"), r.ok && slurp(card + QStringLiteral("/ext/apps/Games/snake_game.fap")) == snake, r.message, failures);

    // 4. snapshots
    r = await(c, QStringLiteral("snapshot"), [&]() { c.takeSnapshot(QStringLiteral("first")); });
    check(QStringLiteral("snapshot one"), r.ok, r.message, failures);
    const auto garage = slurp(card + QStringLiteral("/ext/subghz/garage.sub"));
    put(card, QStringLiteral("/ext/subghz/garage.sub"), garage + "# changed\n");
    put(card, QStringLiteral("/ext/infrared/tv.ir"), slurp(QDir(fixtures).filePath(QStringLiteral("captures/ext/infrared/tv.ir"))));
    r = await(c, QStringLiteral("snapshot"), [&]() { c.takeSnapshot(QStringLiteral("second")); });
    check(QStringLiteral("snapshot two downloads only what changed"), r.ok && r.message.contains(QStringLiteral(" 2 downloaded")), r.message, failures);
    await(c, QStringLiteral("snapshots"), [&]() { c.refreshSnapshots(); });
    const auto snaps = c.snapshots();
    check(QStringLiteral("two snapshots listed"), snaps.size() == 2, QString(), failures);
    const auto first = snaps.value(0).toMap().value(QStringLiteral("id")).toString();
    const auto second = snaps.value(1).toMap().value(QStringLiteral("id")).toString();
    r = await(c, QStringLiteral("diff"), [&]() { c.diffSnapshot(first, second); });
    check(QStringLiteral("diff between snapshots"), r.ok && c.snapshotDiff().value(QStringLiteral("added")).toList().size() == 1
          && c.snapshotDiff().value(QStringLiteral("changed")).toList().size() == 1, r.message, failures);
    QFile::remove(card + QStringLiteral("/ext/nfc/badge.nfc"));
    r = await(c, QStringLiteral("diff"), [&]() { c.diffSnapshot(second, QString()); });
    check(QStringLiteral("diff with the card now"), r.ok && c.snapshotDiff().value(QStringLiteral("removed")).toList().size() == 1
          && c.snapshotDiff().value(QStringLiteral("unknown")).toList().isEmpty(), r.message, failures);
    r = await(c, QStringLiteral("restorePlan"), [&]() { c.planRestore(first, QString()); });
    const auto rp = c.restorePlan();
    check(QStringLiteral("restore plan lists one create and one overwrite"), r.ok && rp.value(QStringLiteral("creates")).toInt() == 1
          && rp.value(QStringLiteral("overwrites")).toInt() == 1, r.message, failures);
    r = await(c, QStringLiteral("restore"), [&]() { c.runRestore(); });
    check(QStringLiteral("restore puts both files back"), r.ok && slurp(card + QStringLiteral("/ext/subghz/garage.sub")) == garage
          && QFileInfo(card + QStringLiteral("/ext/nfc/badge.nfc")).isFile(), r.message, failures);
    r = await(c, QStringLiteral("restorePlan"), [&]() { c.planRestore(first, QStringLiteral("/ext/subghz/garage.sub")); });
    check(QStringLiteral("restoring one unchanged file does nothing"), r.ok && c.restorePlan().value(QStringLiteral("same")).toInt() == 1
          && c.restorePlan().value(QStringLiteral("items")).toList().size() == 1, r.message, failures);

    // 5. capture library + Sarina API
    r = await(c, QStringLiteral("captureIndex"), [&]() { c.readCapturesFromCard(); });
    await(c, QStringLiteral("captureSearch"), [&]() { c.searchCaptures(QString()); });
    check(QStringLiteral("capture library from card + snapshots, assets left out"), r.ok && c.captures().size() == 4, QString::number(c.captures().size()), failures);
    const auto api = bridge.handleApiRequest(QJsonObject { {QStringLiteral("cmd"), QStringLiteral("capture.library")}, {QStringLiteral("query"), QStringLiteral("proto:princeton")} });
    const auto found = api.value(QStringLiteral("captures")).toArray();
    check(QStringLiteral("API capture.library (both versions of garage.sub)"), api.value(QStringLiteral("ok")).toBool() && found.size() == 2, QString::fromUtf8(QJsonDocument(api).toJson(QJsonDocument::Compact)).left(200), failures);
    const auto id = found.at(0).toObject().value(QStringLiteral("id")).toString();
    const auto ex = bridge.handleApiRequest(QJsonObject { {QStringLiteral("cmd"), QStringLiteral("capture.explain")}, {QStringLiteral("id"), id} });
    check(QStringLiteral("API capture.explain"), ex.value(QStringLiteral("ok")).toBool() && ex.value(QStringLiteral("explanation")).toString().contains(QStringLiteral("fixed-code")),
          ex.value(QStringLiteral("explanation")).toString().left(120), failures);
    const auto leak = QString::fromUtf8(QJsonDocument(api).toJson()) + QString::fromUtf8(QJsonDocument(ex).toJson());
    check(QStringLiteral("API output has no key payload or local paths"), !leak.contains(QStringLiteral("95 D5 D4")) && !leak.contains(home.path()), QString(), failures);
    const auto bad = bridge.handleApiRequest(QJsonObject { {QStringLiteral("cmd"), QStringLiteral("capture.explain")}, {QStringLiteral("id"), QStringLiteral("../../etc")} });
    check(QStringLiteral("API rejects a bad id"), !bad.value(QStringLiteral("ok")).toBool(), bad.value(QStringLiteral("error")).toString(), failures);
    await(c, QStringLiteral("captureTag"), [&]() { c.tagCapture(id, {QStringLiteral("garage")}, {}); });
    await(c, QStringLiteral("captureSearch"), [&]() { c.searchCaptures(QStringLiteral("tag:garage")); });
    check(QStringLiteral("tag and search by tag"), c.captures().size() == 1, QString(), failures);

    // 6. the fake card refuses unsafe paths like the real one
    LocalDeviceStorage s(card);
    bool wrote = true;
    QEventLoop l2;
    s.write(QDir(fixtures).filePath(QStringLiteral("snake_game.fap")), QStringLiteral("/ext/../escape.fap"), [&](bool ok, const QString &) { wrote = ok; l2.quit(); });
    l2.exec();
    check(QStringLiteral("device paths with .. are refused"), !wrote && !QFileInfo(card + QStringLiteral("/escape.fap")).exists(), QString(), failures);

    out() << (failures ? QStringLiteral("%1 check(s) failed\n").arg(failures) : QStringLiteral("All checks passed\n"));
    out().flush();
    return failures ? 1 : 0;
}

int ArgusCli::run(QCoreApplication &app, const QStringList &args)
{
    const auto cmd = args.value(0);
    QString fakeCard, label, packageArg;
    for(int i = 1; i + 1 < args.size(); i += 2) {
        if(args[i] == QLatin1String("--fake-card")) {
            fakeCard = args[i + 1];
        } else if(args[i] == QLatin1String("--label")) {
            label = args[i + 1];
        } else if(args[i] == QLatin1String("--package")) {
            packageArg = args[i + 1];
        }
    }
    static const QStringList commands {QStringLiteral("checkup"), QStringLiteral("snapshot"), QStringLiteral("captures"), QStringLiteral("api")};
    if(!commands.contains(cmd)) {
        out() << "Usage: qFlipper-cli chymaera argus <checkup|snapshot|captures|api> [--fake-card DIR] [--package PKG] [--label TEXT]\n"
              << "  checkup   list the apps on the card the firmware cannot load (with --package: compatible copies)\n"
              << "  snapshot  take an SD card snapshot into ~/.local/share/argus/snapshots\n"
              << "  captures  read the capture folders and rebuild the capture library\n"
              << "  api       print the firmware API Argus reads from the device\n";
        out().flush();
        return 1;
    }

    Chymaera::Bridge bridge;
    auto &c = *bridge.argus();
    QObject::connect(&c, &ArgusController::statusChanged, [&c]() {
        out() << "  … " << c.status() << "\n";
        out().flush();
    });

    auto go = [&]() -> int {
        QEventLoop settle;
        QTimer::singleShot(1500, &settle, &QEventLoop::quit); // the API probe
        settle.exec();
        Result r;
        if(cmd == QLatin1String("api")) {
            out() << c.firmwareApi() << (c.isFirmwareApiFromDevice() ? " (read from the device)\n" : " (default; could not be read)\n");
            return 0;
        } else if(cmd == QLatin1String("checkup")) {
            r = await(c, QStringLiteral("checkup"), [&]() { c.runCheckup(packageArg.isEmpty() ? QStringList() : QStringList {packageArg}); }, 1800000);
            out() << QJsonDocument(QJsonObject::fromVariantMap(c.checkupReport())).toJson();
        } else if(cmd == QLatin1String("snapshot")) {
            r = await(c, QStringLiteral("snapshot"), [&]() { c.takeSnapshot(label); }, 6 * 3600000);
        } else {
            r = await(c, QStringLiteral("captureIndex"), [&]() { c.readCapturesFromCard(); }, 1800000);
        }
        out() << (r.ok ? "OK: " : "FAILED: ") << r.message << "\n";
        out().flush();
        return r.ok ? 0 : 1;
    };

    if(!fakeCard.isEmpty()) {
        c.useFakeCard(fakeCard);
        return go();
    }

    ApplicationBackend backend;
    int rc = 2;
    bool started = false;
    QObject::connect(&backend, &ApplicationBackend::backendStateChanged, &app, [&]() {
        const auto state = backend.backendState();
        if(!started && state == ApplicationBackend::BackendState::Ready && backend.device()) {
            started = true;
            bridge.attachDevice(backend.device());
            rc = go();
            app.quit();
        } else if(state == ApplicationBackend::BackendState::ErrorOccured) {
            out() << "Device error.\n";
            rc = 1;
            app.quit();
        }
    });
    out() << "Waiting for a Flipper…\n";
    out().flush();
    QTimer::singleShot(300000, &app, [&]() {
        if(!started) {
            out() << "Timed out waiting for a ready device.\n";
            app.quit();
        }
    });
    app.exec();
    return rc;
}
