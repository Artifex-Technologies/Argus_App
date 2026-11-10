#include "arguscontroller.h"

#include <memory>

#include <QDir>
#include <QUrl>
#include <QFile>
#include <QTimer>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QStandardPaths>
#include <QCoreApplication>

#include "argustools.h"
#include "devicestorage.h"
#include "chymaera/sync/capturesync.h"

using namespace Argus;

namespace {

const auto DEFAULT_API = QStringLiteral("88.2/7"); // Argus firmware ARG-1.5.1rN, Flipper Zero (f7)

// The capture folders the library reads from the card (matches argus-tools capture::FOLDERS).
const QList<QPair<QString, QString>> CAPTURE_FOLDERS = {
    {QStringLiteral("/ext/subghz"), QStringLiteral(".sub")},
    {QStringLiteral("/ext/infrared"), QStringLiteral(".ir")},
    {QStringLiteral("/ext/nfc"), QStringLiteral(".nfc")},
    {QStringLiteral("/ext/lfrfid"), QStringLiteral(".rfid")},
    {QStringLiteral("/ext/ibutton"), QStringLiteral(".ibtn")},
};

constexpr qint64 MAX_APP_BYTES = 8 << 20;
constexpr qint64 MAX_CAPTURE_BYTES = 4 << 20;

/* Run step() for each item in turn (each step calls next when it is done), then done(). */
template<typename T>
void forEachAsync(const QList<T> &items, std::function<void(const T &, int, std::function<void()>)> step, std::function<void()> done)
{
    auto index = std::make_shared<int>(0);
    auto list = std::make_shared<QList<T>>(items);
    auto next = std::make_shared<std::function<void()>>();
    *next = [=]() {
        if(*index >= list->size()) {
            done();
            *next = nullptr; // break the cycle
            return;
        }
        const int i = (*index)++;
        step(list->at(i), i, *next);
    };
    (*next)();
}

QString localPathOf(const QString &pathOrUrl)
{
    const QUrl url(pathOrUrl);
    return url.isLocalFile() ? url.toLocalFile() : pathOrUrl;
}

bool writeJson(const QString &path, const QJsonObject &obj)
{
    QFile f(path);
    if(!f.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
        return false;
    }
    f.write(QJsonDocument(obj).toJson(QJsonDocument::Compact));
    return true;
}

bool isUnder(const QString &path, const QString &root)
{
    return path == root || path.startsWith(root + QLatin1Char('/'));
}

QString parentOf(const QString &path)
{
    return path.left(path.lastIndexOf(QLatin1Char('/')));
}

} // namespace

ArgusController::ArgusController(QObject *parent):
    QObject(parent),
    m_firmwareApi(DEFAULT_API),
    m_apiFromDevice(false),
    m_busy(false),
    m_progress(-1)
{
    // try the features without a Flipper: a folder that plays the card (<folder>/ext/...)
    const auto fake = qEnvironmentVariable("ARGUS_FAKE_CARD");
    if(!fake.isEmpty() && QFileInfo(fake).isDir()) {
        useFakeCard(fake);
    }
}

ArgusController::~ArgusController() = default;

bool ArgusController::isAvailable() const { return ArgusTools::isAvailable(); }
bool ArgusController::isBusy() const { return m_busy; }
QString ArgusController::status() const { return m_status; }
double ArgusController::progress() const { return m_progress; }
bool ArgusController::isCardReady() const { return !m_storage.isNull(); }
QString ArgusController::cardName() const { return m_storage.isNull() ? QString() : m_storage->name(); }
QString ArgusController::firmwareApi() const { return m_firmwareApi; }
bool ArgusController::isFirmwareApiFromDevice() const { return m_apiFromDevice; }
QVariantMap ArgusController::packageReport() const { return m_packageReport; }
QVariantList ArgusController::installPlan() const { return m_installPlan; }
QVariantMap ArgusController::checkupReport() const { return m_checkupReport; }
QVariantMap ArgusController::rebuildReport() const { return m_rebuildReport; }
QVariantList ArgusController::snapshots() const { return m_snapshots; }
QVariantMap ArgusController::snapshotDiff() const { return m_snapshotDiff; }
QVariantMap ArgusController::restorePlan() const { return m_restorePlan; }
QVariantList ArgusController::captures() const { return m_captures; }
QVariantMap ArgusController::captureDetail() const { return m_captureDetail; }
QVariantList ArgusController::duplicates() const { return m_duplicates; }
bool ArgusController::isSmokeTest() const { return qEnvironmentVariableIsSet("ARGUS_QML_SMOKE"); }
QString ArgusController::smokeArg() const { return qEnvironmentVariable("ARGUS_QML_SMOKE"); }

QString ArgusController::chymaeraPullsDir()
{
    const auto base = QStandardPaths::writableLocation(QStandardPaths::GenericDataLocation);
    return QDir(base).filePath(QStringLiteral("Argus/chymaera/pulls"));
}

QString ArgusController::apiArg() const
{
    return m_firmwareApi.isEmpty() ? DEFAULT_API : m_firmwareApi;
}

void ArgusController::setStorage(DeviceStorage *storage)
{
    if(m_storage) {
        m_storage->deleteLater();
    }
    m_storage = storage;
    m_firmwareApi = DEFAULT_API;
    m_apiFromDevice = false;
    if(storage) {
        storage->setParent(this);
        storage->firmwareApi([this, storage](const QString &api) {
            if(m_storage == storage && !api.isEmpty()) {
                m_firmwareApi = api;
                m_apiFromDevice = true;
                emit cardChanged();
            }
        });
    }
    emit cardChanged();
}

void ArgusController::attachDevice(Flipper::FlipperZero *device)
{
    setStorage(device ? new RpcDeviceStorage(device) : nullptr);
}

void ArgusController::detachDevice()
{
    setStorage(nullptr);
}

void ArgusController::useFakeCard(const QString &folder, const QString &api)
{
    setStorage(new LocalDeviceStorage(localPathOf(folder), api));
}

// ---- plumbing --------------------------------------------------------------------------------

void ArgusController::setStatus(const QString &text, double progress)
{
    m_status = text;
    m_progress = progress;
    emit statusChanged();
}

bool ArgusController::begin(const QString &op, const QString &text, bool needsCard)
{
    if(m_busy) {
        QTimer::singleShot(0, this, [=, this]() { emit operationFinished(op, false, QStringLiteral("another operation is running")); });
        return false;
    }
    if(needsCard && m_storage.isNull()) {
        QTimer::singleShot(0, this, [=, this]() { emit operationFinished(op, false, QStringLiteral("no Flipper connected")); });
        return false;
    }
    m_busy = true;
    emit busyChanged();
    setStatus(text, -1);
    return true;
}

void ArgusController::end(const QString &op, bool ok, const QString &message)
{
    if(m_busy) {
        m_busy = false;
        emit busyChanged();
    }
    setStatus(message, -1);
    emit operationFinished(op, ok, message);
}

void ArgusController::runTool(const QString &op, const QStringList &args, std::function<void(const QJsonObject &)> onOk,
                              bool exclusive, int timeoutMs)
{
    auto *job = new ArgusToolsJob(args, this);
    connect(job, &ArgusToolsJob::progress, this, [this](const QString &m) { setStatus(m, m_progress); });
    connect(job, &ArgusToolsJob::finished, this, [=, this](const QJsonObject &r) {
        if(!r.value(QStringLiteral("ok")).toBool()) {
            const auto msg = ArgusTools::errorText(r);
            if(exclusive) {
                end(op, false, msg);
            } else {
                setStatus(msg);
                emit operationFinished(op, false, msg);
            }
            return;
        }
        onOk(r);
    });
    job->start(timeoutMs);
}

void ArgusController::downloadAll(const QStringList &paths, const QString &staging, std::function<void(int)> done)
{
    auto failed = std::make_shared<int>(0);
    const int total = paths.size();
    forEachAsync<QString>(paths, [=, this](const QString &remote, int i, std::function<void()> next) {
        setStatus(QStringLiteral("Reading %1 (%2 of %3)").arg(remote).arg(i + 1).arg(total), total ? double(i) / total : -1);
        // names come from the device: refuse any that would lead outside the staging folder
        const auto local = Chymaera::stagedPath(staging, remote);
        if(local.isEmpty() || !QDir().mkpath(QFileInfo(local).absolutePath()) || m_storage.isNull()) {
            ++*failed;
            next();
            return;
        }
        m_storage->read(remote, local, [=, this](bool ok, const QString &) {
            if(!ok) {
                ++*failed;
                QFile::remove(local);
            }
            next();
        });
    }, [=, this]() { done(*failed); });
}

/* Write items ({local, remote, md5}) to the card: parent folder first, then the file, then compare
 * the card's MD5 with the local one. */
void ArgusController::writeAll(const QVariantList &items, std::function<void(int, int, QStringList)> done)
{
    auto written = std::make_shared<int>(0);
    auto failed = std::make_shared<int>(0);
    auto errors = std::make_shared<QStringList>();
    const int total = items.size();
    forEachAsync<QVariant>(items, [=, this](const QVariant &v, int i, std::function<void()> next) {
        const auto item = v.toMap();
        const auto remote = item.value(QStringLiteral("remote")).toString();
        const auto local = item.value(QStringLiteral("local")).toString();
        const auto md5 = item.value(QStringLiteral("md5")).toString();
        setStatus(QStringLiteral("Writing %1 (%2 of %3)").arg(remote).arg(i + 1).arg(total), total ? double(i) / total : -1);
        auto fail = [=, this](const QString &why) {
            ++*failed;
            errors->append(QStringLiteral("%1: %2").arg(remote, why));
            next();
        };
        if(m_storage.isNull() || !isSafeDevicePath(remote) || !QFileInfo(local).isFile()) {
            fail(QStringLiteral("not writable"));
            return;
        }
        m_storage->mkpath(parentOf(remote), [=, this](bool ok, const QString &err) {
            if(!ok || m_storage.isNull()) {
                fail(QStringLiteral("cannot create the folder (%1)").arg(err));
                return;
            }
            m_storage->write(local, remote, [=, this](bool ok, const QString &err) {
                if(!ok || m_storage.isNull()) {
                    fail(err.isEmpty() ? QStringLiteral("write failed") : err);
                    return;
                }
                if(md5.isEmpty()) {
                    ++*written;
                    next();
                    return;
                }
                m_storage->md5(remote, [=, this](bool ok, const QString &got) {
                    if(ok && got.compare(md5, Qt::CaseInsensitive) == 0) {
                        ++*written;
                        next();
                    } else {
                        fail(QStringLiteral("the card's copy does not match (MD5)"));
                    }
                });
            });
        });
    }, [=, this]() { done(*written, *failed, *errors); });
}

// ---- apps ------------------------------------------------------------------------------------

void ArgusController::scanPackage(const QString &pathOrUrl)
{
    if(!begin(QStringLiteral("package"), QStringLiteral("Opening the package…"), false)) {
        return;
    }
    const auto path = localPathOf(pathOrUrl);
    runTool(QStringLiteral("package"),
            {QStringLiteral("package"), QStringLiteral("scan"), path, QStringLiteral("--out"), ArgusTools::appsDir(),
             QStringLiteral("--api"), apiArg()},
            [=, this](const QJsonObject &r) {
        m_packageReport = r.toVariantMap();
        emit packageReportChanged();
        const auto apps = r.value(QStringLiteral("apps")).toArray();
        int compatible = 0;
        for(const auto &a : apps) {
            compatible += a.toObject().value(QStringLiteral("compat")).toObject().value(QStringLiteral("status")).toString() == QLatin1String("compatible");
        }
        end(QStringLiteral("package"), true, QStringLiteral("%1 apps in the package, %2 compatible with API %3%4")
            .arg(apps.size()).arg(compatible).arg(apiArg(),
                 r.value(QStringLiteral("incomplete")).toBool() ? QStringLiteral(" (incomplete: a limit was hit)") : QString()));
    }, true);
}

void ArgusController::prepareInstall(const QVariantList &apps, bool force)
{
    if(!begin(QStringLiteral("installPlan"), QStringLiteral("Checking the card…"))) {
        return;
    }
    QVariantList plan;
    for(const auto &v : apps) {
        const auto a = v.toMap();
        const auto compat = a.value(QStringLiteral("compat")).toMap();
        const auto status = compat.value(QStringLiteral("status")).toString();
        QString remote = a.value(QStringLiteral("target")).toString();
        if(remote.isEmpty()) {
            remote = QStringLiteral("/ext/apps/%1/%2").arg(a.value(QStringLiteral("category")).toString(),
                                                           a.value(QStringLiteral("file_name")).toString());
        }
        QVariantMap item {
            {QStringLiteral("local"), a.value(QStringLiteral("stored")).toString()},
            {QStringLiteral("remote"), remote},
            {QStringLiteral("name"), a.value(QStringLiteral("manifest")).toMap().value(QStringLiteral("name")).toString()},
            {QStringLiteral("md5"), a.value(QStringLiteral("md5")).toString()},
            {QStringLiteral("status"), status},
            {QStringLiteral("exists"), false},
            {QStringLiteral("overwrite"), false},
            {QStringLiteral("refused"), false},
            {QStringLiteral("reason"), compat.value(QStringLiteral("reason")).toString()},
        };
        QString refusal;
        if(compat.value(QStringLiteral("blocked")).toBool()) {
            refusal = QStringLiteral("refused: %1").arg(compat.value(QStringLiteral("reason")).toString());
        } else if(status != QLatin1String("compatible") && !force) {
            refusal = QStringLiteral("not compatible with API %1 (%2); tick Force to install anyway")
                          .arg(apiArg(), compat.value(QStringLiteral("code")).toString());
        } else if(!isSafeDevicePath(remote) || !remote.startsWith(QLatin1String("/ext/apps/")) || !remote.endsWith(QLatin1String(".fap"), Qt::CaseInsensitive)) {
            refusal = QStringLiteral("refused: not a path under /ext/apps");
        } else if(!QFileInfo(item.value(QStringLiteral("local")).toString()).isFile()) {
            refusal = QStringLiteral("refused: the extracted copy is missing");
        }
        if(!refusal.isEmpty()) {
            item[QStringLiteral("refused")] = true;
            item[QStringLiteral("reason")] = refusal;
        }
        plan.append(item);
    }
    auto result = std::make_shared<QVariantList>(plan);
    QList<int> indexes;
    for(int i = 0; i < plan.size(); ++i) {
        indexes.append(i);
    }
    forEachAsync<int>(indexes, [=, this](const int &i, int, std::function<void()> next) {
        auto item = (*result)[i].toMap();
        if(item.value(QStringLiteral("refused")).toBool() || m_storage.isNull()) {
            next();
            return;
        }
        m_storage->stat(item.value(QStringLiteral("remote")).toString(), [=, this](bool exists, bool isDir, qint64 size) {
            auto it = (*result)[i].toMap();
            it[QStringLiteral("exists")] = exists;
            it[QStringLiteral("existingSize")] = size;
            if(exists && isDir) {
                it[QStringLiteral("refused")] = true;
                it[QStringLiteral("reason")] = QStringLiteral("refused: a folder has that name on the card");
            }
            (*result)[i] = it;
            next();
        });
    }, [=, this]() {
        m_installPlan = *result;
        emit installPlanChanged();
        int ready = 0, existing = 0, refused = 0;
        for(const auto &v : *result) {
            const auto m = v.toMap();
            if(m.value(QStringLiteral("refused")).toBool()) {
                ++refused;
            } else if(m.value(QStringLiteral("exists")).toBool()) {
                ++existing;
            } else {
                ++ready;
            }
        }
        end(QStringLiteral("installPlan"), true,
            QStringLiteral("%1 new, %2 already on the card (asks before overwriting), %3 refused").arg(ready).arg(existing).arg(refused));
    });
}

void ArgusController::runInstall(const QVariantList &items)
{
    if(!begin(QStringLiteral("install"), QStringLiteral("Installing…"))) {
        return;
    }
    QVariantList todo;
    int kept = 0, refused = 0;
    for(const auto &v : items) {
        const auto m = v.toMap();
        if(m.value(QStringLiteral("refused")).toBool()) {
            ++refused;
        } else if(m.value(QStringLiteral("exists")).toBool() && !m.value(QStringLiteral("overwrite")).toBool()) {
            ++kept;
        } else {
            todo.append(m);
        }
    }
    writeAll(todo, [=, this](int written, int failed, QStringList errors) {
        m_installPlan.clear();
        emit installPlanChanged();
        auto msg = QStringLiteral("Installed %1, kept %2 existing, refused %3, failed %4").arg(written).arg(kept).arg(refused).arg(failed);
        if(!errors.isEmpty()) {
            msg += QStringLiteral(": ") + errors.mid(0, 5).join(QStringLiteral("; "));
        }
        end(QStringLiteral("install"), failed == 0, msg);
    });
}

void ArgusController::runCheckup(const QStringList &packages)
{
    if(!begin(QStringLiteral("checkup"), QStringLiteral("Listing the apps on the card…"))) {
        return;
    }
    const auto staging = ArgusTools::newStagingDir(QStringLiteral("checkup"));
    m_storage->listTree(QStringLiteral("/ext/apps"), [=, this](bool ok, const QString &, const QList<RemoteEntry> &entries) {
        QStringList faps;
        if(ok) {
            for(const auto &e : entries) {
                if(!e.isDir && e.path.endsWith(QLatin1String(".fap"), Qt::CaseInsensitive) && e.size <= MAX_APP_BYTES && isSafeDevicePath(e.path)) {
                    faps.append(e.path);
                }
            }
        }
        downloadAll(faps, staging, [=, this](int failed) {
            QStringList args {QStringLiteral("checkup"), QStringLiteral("--dir"), staging, QStringLiteral("--api"), apiArg(),
                              QStringLiteral("--out"), ArgusTools::appsDir()};
            for(const auto &p : packages) {
                args << QStringLiteral("--package") << localPathOf(p);
            }
            setStatus(QStringLiteral("Reading %1 app manifests…").arg(faps.size()));
            runTool(QStringLiteral("checkup"), args, [=, this](const QJsonObject &r) {
                QDir(staging).removeRecursively();
                m_checkupReport = r.toVariantMap();
                emit checkupReportChanged();
                end(QStringLiteral("checkup"), true,
                    QStringLiteral("%1 apps on the card: %2 will not load on API %3, %4 unknown%5")
                        .arg(r.value(QStringLiteral("apps")).toArray().size())
                        .arg(r.value(QStringLiteral("incompatible")).toInt())
                        .arg(apiArg())
                        .arg(r.value(QStringLiteral("unknown")).toInt())
                        .arg(failed ? QStringLiteral(" (%1 could not be read)").arg(failed) : QString()));
            }, true);
        });
    });
}

void ArgusController::rebuild(const QString &source, const QString &gitRef, const QString &subdir)
{
    if(!begin(QStringLiteral("rebuild"), QStringLiteral("Starting the Limbo build…"), false)) {
        return;
    }
    QStringList args {QStringLiteral("limbo"), QStringLiteral("build")};
    if(source.startsWith(QLatin1String("https://"))) {
        args << QStringLiteral("--git") << source;
        if(!gitRef.isEmpty()) {
            args << QStringLiteral("--ref") << gitRef;
        }
        if(!subdir.isEmpty()) {
            args << QStringLiteral("--subdir") << subdir;
        }
    } else {
        args << QStringLiteral("--source") << localPathOf(source);
    }
    args << QStringLiteral("--out") << QDir(ArgusTools::appsDir()).filePath(QStringLiteral("built"))
         << QStringLiteral("--api") << apiArg();
    runTool(QStringLiteral("rebuild"), args, [=, this](const QJsonObject &r) {
        m_rebuildReport = r.toVariantMap();
        emit rebuildReportChanged();
        end(QStringLiteral("rebuild"), true, QStringLiteral("Built %1 app(s) in Limbo").arg(r.value(QStringLiteral("faps")).toArray().size()));
    }, true, 40 * 60 * 1000);
}

// ---- snapshots -------------------------------------------------------------------------------

void ArgusController::refreshSnapshots()
{
    runTool(QStringLiteral("snapshots"), {QStringLiteral("snapshot"), QStringLiteral("list"), QStringLiteral("--store"), ArgusTools::snapshotsDir()},
            [=, this](const QJsonObject &r) {
        m_snapshots = r.value(QStringLiteral("snapshots")).toArray().toVariantList();
        emit snapshotsChanged();
        emit operationFinished(QStringLiteral("snapshots"), true, QString());
    }, false);
}

/* List the card under /ext as argus-tools' Listing ({root, files:[{path,size}], dirs}). */
static QJsonObject listingOf(const QList<RemoteEntry> &entries)
{
    QJsonArray files, dirs;
    for(const auto &e : entries) {
        if(!isSafeDevicePath(e.path)) {
            continue;
        }
        if(e.isDir) {
            dirs.append(e.path);
        } else {
            files.append(QJsonObject { {QStringLiteral("path"), e.path}, {QStringLiteral("size"), e.size} });
        }
    }
    return QJsonObject { {QStringLiteral("root"), QStringLiteral("/ext")}, {QStringLiteral("files"), files}, {QStringLiteral("dirs"), dirs} };
}

void ArgusController::takeSnapshot(const QString &label)
{
    if(!begin(QStringLiteral("snapshot"), QStringLiteral("Listing the card…"))) {
        return;
    }
    const auto staging = ArgusTools::newStagingDir(QStringLiteral("snapshot"));
    const auto listingPath = QDir(staging).filePath(QStringLiteral("listing.json"));
    const auto mirror = QDir(staging).filePath(QStringLiteral("card"));
    auto fail = [=, this](const QString &why) {
        QDir(staging).removeRecursively();
        end(QStringLiteral("snapshot"), false, why);
    };
    m_storage->listTree(QStringLiteral("/ext"), [=, this](bool ok, const QString &err, const QList<RemoteEntry> &entries) {
        if(!ok) {
            fail(QStringLiteral("cannot list the card: %1").arg(err));
            return;
        }
        auto listing = std::make_shared<QJsonObject>(listingOf(entries));
        writeJson(listingPath, *listing);
        runTool(QStringLiteral("snapshot"), {QStringLiteral("snapshot"), QStringLiteral("plan"), QStringLiteral("--store"), ArgusTools::snapshotsDir(),
                                              QStringLiteral("--listing"), listingPath},
                [=, this](const QJsonObject &plan) {
            QStringList download;
            for(const auto &p : plan.value(QStringLiteral("download")).toArray()) {
                download.append(p.toString());
            }
            QList<QPair<QString, QString>> verify;
            for(const auto &v : plan.value(QStringLiteral("verify_md5")).toArray()) {
                const auto o = v.toObject();
                verify.append({o.value(QStringLiteral("path")).toString(), o.value(QStringLiteral("md5")).toString()});
            }
            auto md5s = std::make_shared<QHash<QString, QString>>();
            auto toDownload = std::make_shared<QStringList>(download);
            const int total = verify.size();
            forEachAsync<QPair<QString, QString>>(verify, [=, this](const QPair<QString, QString> &v, int i, std::function<void()> next) {
                setStatus(QStringLiteral("Checking unchanged files (%1 of %2)").arg(i + 1).arg(total), total ? double(i) / total : -1);
                if(m_storage.isNull()) {
                    next();
                    return;
                }
                m_storage->md5(v.first, [=, this](bool ok, const QString &md5) {
                    if(ok) {
                        md5s->insert(v.first, md5);
                    }
                    if(!ok || md5.compare(v.second, Qt::CaseInsensitive) != 0) {
                        toDownload->append(v.first);
                    }
                    next();
                });
            }, [=, this]() {
                downloadAll(*toDownload, mirror, [=, this](int failed) {
                    QJsonArray files;
                    for(const auto &f : listing->value(QStringLiteral("files")).toArray()) {
                        auto o = f.toObject();
                        const auto p = o.value(QStringLiteral("path")).toString();
                        if(md5s->contains(p)) {
                            o.insert(QStringLiteral("md5"), md5s->value(p));
                        }
                        files.append(o);
                    }
                    listing->insert(QStringLiteral("files"), files);
                    writeJson(listingPath, *listing);
                    setStatus(QStringLiteral("Storing the snapshot…"));
                    const QJsonObject device { {QStringLiteral("card"), cardName()}, {QStringLiteral("firmware_api"), apiArg()} };
                    runTool(QStringLiteral("snapshot"),
                            {QStringLiteral("snapshot"), QStringLiteral("create"), QStringLiteral("--store"), ArgusTools::snapshotsDir(),
                             QStringLiteral("--listing"), listingPath, QStringLiteral("--staging"), mirror,
                             QStringLiteral("--label"), label,
                             QStringLiteral("--device"), QString::fromUtf8(QJsonDocument(device).toJson(QJsonDocument::Compact))},
                            [=, this](const QJsonObject &r) {
                        QDir(staging).removeRecursively();
                        const auto s = r.value(QStringLiteral("snapshot")).toObject();
                        end(QStringLiteral("snapshot"), true,
                            QStringLiteral("Snapshot %1: %2 files, %3 downloaded, %4 confirmed unchanged by MD5, %5 skipped%6")
                                .arg(s.value(QStringLiteral("id")).toString())
                                .arg(s.value(QStringLiteral("file_count")).toInt())
                                .arg(toDownload->size() - failed)
                                .arg(total - (toDownload->size() - download.size()))
                                .arg(s.value(QStringLiteral("skipped")).toInt())
                                .arg(failed ? QStringLiteral(" (%1 could not be read)").arg(failed) : QString()));
                        refreshSnapshots();
                    }, true);
                });
            });
        }, true);
    });
}

void ArgusController::diffSnapshot(const QString &id, const QString &otherId)
{
    if(!otherId.isEmpty()) {
        if(!begin(QStringLiteral("diff"), QStringLiteral("Comparing snapshots…"), false)) {
            return;
        }
        runTool(QStringLiteral("diff"), {QStringLiteral("snapshot"), QStringLiteral("diff"), QStringLiteral("--store"), ArgusTools::snapshotsDir(),
                                         QStringLiteral("--from"), id, QStringLiteral("--to"), otherId},
                [=, this](const QJsonObject &r) {
            m_snapshotDiff = r.toVariantMap();
            emit snapshotDiffChanged();
            end(QStringLiteral("diff"), true, QStringLiteral("Compared %1 with %2").arg(id, otherId));
        }, true);
        return;
    }
    if(!begin(QStringLiteral("diff"), QStringLiteral("Comparing with the card…"))) {
        return;
    }
    const auto staging = ArgusTools::newStagingDir(QStringLiteral("diff"));
    const auto listingPath = QDir(staging).filePath(QStringLiteral("listing.json"));
    runTool(QStringLiteral("diff"), {QStringLiteral("snapshot"), QStringLiteral("show"), QStringLiteral("--store"), ArgusTools::snapshotsDir(),
                                     QStringLiteral("--id"), id},
            [=, this](const QJsonObject &shown) {
        QHash<QString, qint64> sizes;
        for(const auto &f : shown.value(QStringLiteral("snapshot")).toObject().value(QStringLiteral("files")).toArray()) {
            const auto o = f.toObject();
            sizes.insert(o.value(QStringLiteral("path")).toString(), o.value(QStringLiteral("size")).toInteger());
        }
        if(m_storage.isNull()) {
            end(QStringLiteral("diff"), false, QStringLiteral("no Flipper connected"));
            return;
        }
        m_storage->listTree(QStringLiteral("/ext"), [=, this](bool ok, const QString &err, const QList<RemoteEntry> &entries) {
            if(!ok) {
                QDir(staging).removeRecursively();
                end(QStringLiteral("diff"), false, QStringLiteral("cannot list the card: %1").arg(err));
                return;
            }
            auto listing = std::make_shared<QJsonObject>(listingOf(entries));
            // same path and size as in the snapshot: only the card's MD5 can tell
            QStringList same;
            for(const auto &e : entries) {
                if(!e.isDir && sizes.value(e.path, -1) == e.size) {
                    same.append(e.path);
                }
            }
            auto md5s = std::make_shared<QHash<QString, QString>>();
            const int total = same.size();
            forEachAsync<QString>(same, [=, this](const QString &p, int i, std::function<void()> next) {
                setStatus(QStringLiteral("Comparing %1 (%2 of %3)").arg(p).arg(i + 1).arg(total), total ? double(i) / total : -1);
                if(m_storage.isNull()) {
                    next();
                    return;
                }
                m_storage->md5(p, [=, this](bool ok, const QString &md5) {
                    if(ok) {
                        md5s->insert(p, md5);
                    }
                    next();
                });
            }, [=, this]() {
                QJsonArray files;
                for(const auto &f : listing->value(QStringLiteral("files")).toArray()) {
                    auto o = f.toObject();
                    const auto p = o.value(QStringLiteral("path")).toString();
                    if(md5s->contains(p)) {
                        o.insert(QStringLiteral("md5"), md5s->value(p));
                    }
                    files.append(o);
                }
                listing->insert(QStringLiteral("files"), files);
                writeJson(listingPath, *listing);
                runTool(QStringLiteral("diff"), {QStringLiteral("snapshot"), QStringLiteral("diff"), QStringLiteral("--store"), ArgusTools::snapshotsDir(),
                                                 QStringLiteral("--from"), id, QStringLiteral("--listing"), listingPath},
                        [=, this](const QJsonObject &r) {
                    QDir(staging).removeRecursively();
                    m_snapshotDiff = r.toVariantMap();
                    emit snapshotDiffChanged();
                    end(QStringLiteral("diff"), true,
                        QStringLiteral("Since %1: %2 added, %3 removed, %4 changed, %5 unchanged")
                            .arg(id)
                            .arg(r.value(QStringLiteral("added")).toArray().size())
                            .arg(r.value(QStringLiteral("removed")).toArray().size())
                            .arg(r.value(QStringLiteral("changed")).toArray().size())
                            .arg(r.value(QStringLiteral("unchanged")).toInt()));
                }, true);
            });
        });
    }, true);
}

void ArgusController::planRestore(const QString &id, const QString &path)
{
    if(!begin(QStringLiteral("restorePlan"), QStringLiteral("Comparing the snapshot with the card…"))) {
        return;
    }
    if(!path.isEmpty() && !isSafeDevicePath(path)) {
        end(QStringLiteral("restorePlan"), false, QStringLiteral("not a path on the card: %1").arg(path));
        return;
    }
    const auto staging = ArgusTools::newStagingDir(QStringLiteral("restore"));
    const auto listingPath = QDir(staging).filePath(QStringLiteral("listing.json"));
    runTool(QStringLiteral("restorePlan"), {QStringLiteral("snapshot"), QStringLiteral("show"), QStringLiteral("--store"), ArgusTools::snapshotsDir(),
                                            QStringLiteral("--id"), id},
            [=, this](const QJsonObject &shown) {
        QHash<QString, qint64> sizes;
        for(const auto &f : shown.value(QStringLiteral("snapshot")).toObject().value(QStringLiteral("files")).toArray()) {
            const auto o = f.toObject();
            const auto p = o.value(QStringLiteral("path")).toString();
            if(path.isEmpty() || isUnder(p, path)) {
                sizes.insert(p, o.value(QStringLiteral("size")).toInteger());
            }
        }
        if(m_storage.isNull()) {
            end(QStringLiteral("restorePlan"), false, QStringLiteral("no Flipper connected"));
            return;
        }
        m_storage->listTree(QStringLiteral("/ext"), [=, this](bool ok, const QString &err, const QList<RemoteEntry> &entries) {
            if(!ok) {
                QDir(staging).removeRecursively();
                end(QStringLiteral("restorePlan"), false, QStringLiteral("cannot list the card: %1").arg(err));
                return;
            }
            auto listing = std::make_shared<QJsonObject>(listingOf(entries));
            QStringList same;
            for(const auto &e : entries) {
                if(!e.isDir && sizes.value(e.path, -1) == e.size) {
                    same.append(e.path);
                }
            }
            auto md5s = std::make_shared<QHash<QString, QString>>();
            forEachAsync<QString>(same, [=, this](const QString &p, int i, std::function<void()> next) {
                setStatus(QStringLiteral("Comparing %1 (%2 of %3)").arg(p).arg(i + 1).arg(same.size()));
                if(m_storage.isNull()) {
                    next();
                    return;
                }
                m_storage->md5(p, [=, this](bool ok, const QString &md5) {
                    if(ok) {
                        md5s->insert(p, md5);
                    }
                    next();
                });
            }, [=, this]() {
                QJsonArray files;
                for(const auto &f : listing->value(QStringLiteral("files")).toArray()) {
                    auto o = f.toObject();
                    const auto p = o.value(QStringLiteral("path")).toString();
                    if(md5s->contains(p)) {
                        o.insert(QStringLiteral("md5"), md5s->value(p));
                    }
                    files.append(o);
                }
                listing->insert(QStringLiteral("files"), files);
                writeJson(listingPath, *listing);
                QStringList args {QStringLiteral("snapshot"), QStringLiteral("restore-plan"), QStringLiteral("--store"), ArgusTools::snapshotsDir(),
                                  QStringLiteral("--id"), id, QStringLiteral("--listing"), listingPath};
                if(!path.isEmpty()) {
                    args << QStringLiteral("--path") << path;
                }
                runTool(QStringLiteral("restorePlan"), args, [=, this](const QJsonObject &r) {
                    QDir(staging).removeRecursively();
                    m_restorePlan = r.toVariantMap();
                    emit restorePlanChanged();
                    end(QStringLiteral("restorePlan"), true,
                        QStringLiteral("Restoring would create %1 and overwrite %2 file(s); %3 already match")
                            .arg(r.value(QStringLiteral("creates")).toInt())
                            .arg(r.value(QStringLiteral("overwrites")).toInt())
                            .arg(r.value(QStringLiteral("same")).toInt()));
                }, true);
            });
        });
    }, true);
}

void ArgusController::runRestore()
{
    if(m_restorePlan.isEmpty()) {
        emit operationFinished(QStringLiteral("restore"), false, QStringLiteral("plan a restore first"));
        return;
    }
    if(!begin(QStringLiteral("restore"), QStringLiteral("Restoring…"))) {
        return;
    }
    const auto plan = m_restorePlan;
    QStringList mkdirs;
    for(const auto &d : plan.value(QStringLiteral("mkdirs")).toList()) {
        mkdirs.append(d.toString());
    }
    QVariantList items;
    for(const auto &v : plan.value(QStringLiteral("items")).toList()) {
        const auto m = v.toMap();
        if(m.value(QStringLiteral("action")).toString() != QLatin1String("same")) {
            items.append(m);
        }
    }
    forEachAsync<QString>(mkdirs, [=, this](const QString &d, int, std::function<void()> next) {
        if(m_storage.isNull()) {
            next();
            return;
        }
        m_storage->mkpath(d, [=, this](bool, const QString &) { next(); });
    }, [=, this]() {
        writeAll(items, [=, this](int written, int failed, QStringList errors) {
            m_restorePlan.clear();
            emit restorePlanChanged();
            auto msg = QStringLiteral("Restored %1 file(s) from %2, %3 failed").arg(written).arg(plan.value(QStringLiteral("snapshot")).toString()).arg(failed);
            if(!errors.isEmpty()) {
                msg += QStringLiteral(": ") + errors.mid(0, 5).join(QStringLiteral("; "));
            }
            end(QStringLiteral("restore"), failed == 0, msg);
        });
    });
}

void ArgusController::deleteSnapshot(const QString &id)
{
    if(!begin(QStringLiteral("deleteSnapshot"), QStringLiteral("Deleting %1…").arg(id), false)) {
        return;
    }
    runTool(QStringLiteral("deleteSnapshot"), {QStringLiteral("snapshot"), QStringLiteral("delete"), QStringLiteral("--store"), ArgusTools::snapshotsDir(),
                                               QStringLiteral("--id"), id},
            [=, this](const QJsonObject &r) {
        end(QStringLiteral("deleteSnapshot"), true, QStringLiteral("Deleted %1; freed %2 bytes").arg(id).arg(r.value(QStringLiteral("freed_bytes")).toInteger()));
        refreshSnapshots();
    }, true);
}

// ---- captures --------------------------------------------------------------------------------

void ArgusController::readCapturesFromCard()
{
    if(!begin(QStringLiteral("readCaptures"), QStringLiteral("Listing the capture folders…"))) {
        return;
    }
    // a private mirror of the capture folders, replaced on every read
    const auto mirror = QDir(ArgusTools::dataDir()).filePath(QStringLiteral("captures-card"));
    QDir(mirror).removeRecursively();
    QDir().mkpath(mirror);
    QFile::setPermissions(mirror, QFileDevice::ReadOwner | QFileDevice::WriteOwner | QFileDevice::ExeOwner);
    auto paths = std::make_shared<QStringList>();
    forEachAsync<QPair<QString, QString>>(CAPTURE_FOLDERS, [=, this](const QPair<QString, QString> &folder, int, std::function<void()> next) {
        if(m_storage.isNull()) {
            next();
            return;
        }
        m_storage->listTree(folder.first, [=, this](bool ok, const QString &, const QList<RemoteEntry> &entries) {
            if(ok) {
                for(const auto &e : entries) {
                    const auto parts = e.path.split(QLatin1Char('/'));
                    if(!e.isDir && e.size <= MAX_CAPTURE_BYTES && e.path.endsWith(folder.second, Qt::CaseInsensitive)
                       && !parts.contains(QStringLiteral("assets")) && !parts.contains(QStringLiteral(".cache"))) {
                        paths->append(e.path);
                    }
                }
            }
            next();
        });
    }, [=, this]() {
        downloadAll(*paths, mirror, [=, this](int failed) {
            end(QStringLiteral("readCaptures"), failed == 0,
                QStringLiteral("Read %1 capture file(s) from the card%2").arg(paths->size() - failed)
                    .arg(failed ? QStringLiteral(", %1 failed").arg(failed) : QString()));
            buildCaptureLibrary();
        });
    });
}

void ArgusController::buildCaptureLibrary()
{
    if(!begin(QStringLiteral("captureIndex"), QStringLiteral("Indexing captures…"), false)) {
        return;
    }
    const auto mirror = QDir(ArgusTools::dataDir()).filePath(QStringLiteral("captures-card"));
    runTool(QStringLiteral("captureIndex"),
            {QStringLiteral("capture"), QStringLiteral("index"), QStringLiteral("--library"), ArgusTools::captureLibraryDir(),
             QStringLiteral("--snapshots"), ArgusTools::snapshotsDir(), QStringLiteral("--tree"), mirror,
             QStringLiteral("--tree"), chymaeraPullsDir()},
            [=, this](const QJsonObject &r) {
        end(QStringLiteral("captureIndex"), true, QStringLiteral("The library holds %1 capture(s)").arg(r.value(QStringLiteral("captures")).toInt()));
        searchCaptures(m_lastQuery);
    }, true);
}

void ArgusController::searchCaptures(const QString &query)
{
    m_lastQuery = query;
    runTool(QStringLiteral("captureSearch"),
            {QStringLiteral("capture"), QStringLiteral("search"), QStringLiteral("--library"), ArgusTools::captureLibraryDir(),
             QStringLiteral("--query"), query, QStringLiteral("--limit"), QStringLiteral("500")},
            [=, this](const QJsonObject &r) {
        m_captures = r.value(QStringLiteral("captures")).toArray().toVariantList();
        emit capturesChanged();
        emit operationFinished(QStringLiteral("captureSearch"), true,
                               QStringLiteral("%1 of %2").arg(m_captures.size()).arg(r.value(QStringLiteral("total")).toInt()));
    }, false);
}

void ArgusController::explainCapture(const QString &id)
{
    runTool(QStringLiteral("captureExplain"),
            {QStringLiteral("capture"), QStringLiteral("explain"), QStringLiteral("--library"), ArgusTools::captureLibraryDir(), QStringLiteral("--id"), id},
            [=, this](const QJsonObject &r) {
        m_captureDetail = r.toVariantMap();
        emit captureDetailChanged();
        emit operationFinished(QStringLiteral("captureExplain"), true, QString());
    }, false);
}

void ArgusController::tagCapture(const QString &id, const QStringList &add, const QStringList &remove)
{
    QStringList args {QStringLiteral("capture"), QStringLiteral("tag"), QStringLiteral("--library"), ArgusTools::captureLibraryDir(), QStringLiteral("--id"), id};
    for(const auto &t : add) {
        args << QStringLiteral("--add") << t;
    }
    for(const auto &t : remove) {
        args << QStringLiteral("--remove") << t;
    }
    runTool(QStringLiteral("captureTag"), args, [=, this](const QJsonObject &r) {
        emit operationFinished(QStringLiteral("captureTag"), true,
                               r.value(QStringLiteral("tags")).toVariant().toStringList().join(QStringLiteral(", ")));
        explainCapture(id);
        searchCaptures(m_lastQuery);
    }, false);
}

void ArgusController::findDuplicates()
{
    runTool(QStringLiteral("duplicates"), {QStringLiteral("capture"), QStringLiteral("dupes"), QStringLiteral("--library"), ArgusTools::captureLibraryDir()},
            [=, this](const QJsonObject &r) {
        m_duplicates = r.value(QStringLiteral("groups")).toArray().toVariantList();
        emit duplicatesChanged();
        emit operationFinished(QStringLiteral("duplicates"), true, QStringLiteral("%1 group(s)").arg(m_duplicates.size()));
    }, false);
}

// ---- Sarina API ------------------------------------------------------------------------------

/* What the API hands out per capture: header facts, tags and where it was seen. */
static QJsonObject slimCapture(const QJsonObject &c)
{
    QJsonArray paths;
    for(const auto &l : c.value(QStringLiteral("locations")).toArray()) {
        const auto o = l.toObject();
        paths.append(QJsonObject { {QStringLiteral("origin"), o.value(QStringLiteral("origin"))},
                                   {QStringLiteral("source"), o.value(QStringLiteral("origin")).toString() == QLatin1String("snapshot")
                                                                  ? o.value(QStringLiteral("source")) : QJsonValue(QStringLiteral("card"))},
                                   {QStringLiteral("path"), o.value(QStringLiteral("path"))} });
    }
    auto fields = c.value(QStringLiteral("fields")).toObject();
    return QJsonObject {
        {QStringLiteral("id"), c.value(QStringLiteral("id"))},
        {QStringLiteral("sha256"), c.value(QStringLiteral("sha256"))},
        {QStringLiteral("kind"), c.value(QStringLiteral("kind"))},
        {QStringLiteral("name"), c.value(QStringLiteral("name"))},
        {QStringLiteral("summary"), c.value(QStringLiteral("summary"))},
        {QStringLiteral("fields"), fields},
        {QStringLiteral("tags"), c.value(QStringLiteral("tags"))},
        {QStringLiteral("locations"), paths},
    };
}

QJsonObject ArgusController::apiCaptureLibrary(const QString &query, int limit)
{
    const auto r = ArgusTools::runSync({QStringLiteral("capture"), QStringLiteral("search"), QStringLiteral("--library"), ArgusTools::captureLibraryDir(),
                                        QStringLiteral("--query"), query, QStringLiteral("--limit"), QString::number(qBound(1, limit, 500))});
    if(!r.value(QStringLiteral("ok")).toBool()) {
        return r;
    }
    QJsonArray out;
    for(const auto &c : r.value(QStringLiteral("captures")).toArray()) {
        out.append(slimCapture(c.toObject()));
    }
    return QJsonObject { {QStringLiteral("ok"), true}, {QStringLiteral("total"), r.value(QStringLiteral("total"))}, {QStringLiteral("captures"), out} };
}

QJsonObject ArgusController::apiCaptureExplain(const QString &id)
{
    const auto r = ArgusTools::runSync({QStringLiteral("capture"), QStringLiteral("explain"), QStringLiteral("--library"), ArgusTools::captureLibraryDir(),
                                        QStringLiteral("--id"), id});
    if(!r.value(QStringLiteral("ok")).toBool()) {
        return r;
    }
    return QJsonObject { {QStringLiteral("ok"), true},
                         {QStringLiteral("capture"), slimCapture(r.value(QStringLiteral("capture")).toObject())},
                         {QStringLiteral("explanation"), r.value(QStringLiteral("explanation"))} };
}

QString ArgusController::smokeShotPath(const QString &name) const
{
    const auto dir = qEnvironmentVariable("ARGUS_QML_SMOKE_SHOTS");
    if(dir.isEmpty() || name.contains(QLatin1Char('/'))) {
        return QString();
    }
    return QDir(dir).filePath(name);
}

void ArgusController::smokeFinished(bool ok, const QString &detail)
{
    fprintf(stderr, "ARGUS_QML_SMOKE %s %s\n", ok ? "PASS" : "FAIL", qPrintable(detail));
    fflush(stderr);
    QCoreApplication::exit(ok ? 0 : 1);
}
