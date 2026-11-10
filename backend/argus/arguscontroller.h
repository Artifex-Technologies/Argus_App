#pragma once

#include <functional>

#include <QObject>
#include <QPointer>
#include <QVariant>
#include <QJsonObject>
#include <QStringList>

namespace Flipper {
class FlipperZero;
}

namespace Argus {

class DeviceStorage;

/* Argus's own features on top of qFlipper, for the GUI (as Chymaera.argus in QML), the CLI and
 * the Sarina API:
 *
 *  - Apps: open a firmware package as data, list its apps with their API compatibility, install
 *    the ones the user ticks to /ext/apps/<category>/ (never overwriting without asking).
 *  - Check-up: read every app on the card and list the ones the firmware cannot load, with a
 *    compatible copy from a package the user gives, or a rebuild.
 *  - Rebuild: ufbt in disposable Limbo guests (argus-tools/limbo/README.md).
 *  - Snapshots: content-addressed SD card backups, diff, restore one file or all.
 *  - Captures: the capture library (index, search, tags, duplicates, explanations).
 *
 * The logic lives in argus-tools (Rust); this class moves files over the device link and runs it.
 * One device operation at a time (busy). Results arrive as properties plus operationFinished(). */
class ArgusController : public QObject
{
    Q_OBJECT
    Q_PROPERTY(bool available READ isAvailable CONSTANT)
    Q_PROPERTY(bool busy READ isBusy NOTIFY busyChanged)
    Q_PROPERTY(QString status READ status NOTIFY statusChanged)
    Q_PROPERTY(double progress READ progress NOTIFY statusChanged)
    Q_PROPERTY(bool cardReady READ isCardReady NOTIFY cardChanged)
    Q_PROPERTY(QString cardName READ cardName NOTIFY cardChanged)
    Q_PROPERTY(QString firmwareApi READ firmwareApi NOTIFY cardChanged)
    Q_PROPERTY(bool firmwareApiFromDevice READ isFirmwareApiFromDevice NOTIFY cardChanged)
    Q_PROPERTY(QVariantMap packageReport READ packageReport NOTIFY packageReportChanged)
    Q_PROPERTY(QVariantList installPlan READ installPlan NOTIFY installPlanChanged)
    Q_PROPERTY(QVariantMap checkupReport READ checkupReport NOTIFY checkupReportChanged)
    Q_PROPERTY(QVariantMap rebuildReport READ rebuildReport NOTIFY rebuildReportChanged)
    Q_PROPERTY(QVariantList snapshots READ snapshots NOTIFY snapshotsChanged)
    Q_PROPERTY(QVariantMap snapshotDiff READ snapshotDiff NOTIFY snapshotDiffChanged)
    Q_PROPERTY(QVariantMap restorePlan READ restorePlan NOTIFY restorePlanChanged)
    Q_PROPERTY(QVariantList captures READ captures NOTIFY capturesChanged)
    Q_PROPERTY(QVariantMap captureDetail READ captureDetail NOTIFY captureDetailChanged)
    Q_PROPERTY(QVariantList duplicates READ duplicates NOTIFY duplicatesChanged)
    Q_PROPERTY(bool smokeTest READ isSmokeTest CONSTANT)
    Q_PROPERTY(QString smokeArg READ smokeArg CONSTANT)

public:
    explicit ArgusController(QObject *parent = nullptr);
    ~ArgusController() override;

    bool isAvailable() const;
    bool isBusy() const;
    QString status() const;
    double progress() const;
    bool isCardReady() const;
    QString cardName() const;
    QString firmwareApi() const;
    bool isFirmwareApiFromDevice() const;
    QVariantMap packageReport() const;
    QVariantList installPlan() const;
    QVariantMap checkupReport() const;
    QVariantMap rebuildReport() const;
    QVariantList snapshots() const;
    QVariantMap snapshotDiff() const;
    QVariantMap restorePlan() const;
    QVariantList captures() const;
    QVariantMap captureDetail() const;
    QVariantList duplicates() const;
    // ARGUS_QML_SMOKE set: the tools window opens at start, runs through its pages and quits
    // (an offscreen UI check). Its value, if a file, is a package the window scans.
    bool isSmokeTest() const;
    QString smokeArg() const;

    // The card Argus works on: the attached Flipper, or a folder that plays one.
    void attachDevice(Flipper::FlipperZero *device);
    void detachDevice();
    void setStorage(DeviceStorage *storage); // takes ownership
    Q_INVOKABLE void useFakeCard(const QString &folder, const QString &api = QStringLiteral("88.2/7"));

    // Where the Chymaera console's pulls land (indexed into the capture library too).
    static QString chymaeraPullsDir();

    /* Apps */
    Q_INVOKABLE void scanPackage(const QString &pathOrUrl);
    // apps: entries from packageReport.apps / checkupReport.apps[].replacement / rebuildReport.faps,
    // each with stored, file_name, category (or target: a full device path), compat, md5.
    Q_INVOKABLE void prepareInstall(const QVariantList &apps, bool force);
    // items: installPlan with "overwrite" set by the user on the ones that exist.
    Q_INVOKABLE void runInstall(const QVariantList &items);
    Q_INVOKABLE void runCheckup(const QStringList &packages);
    Q_INVOKABLE void rebuild(const QString &source, const QString &gitRef, const QString &subdir);

    /* Snapshots */
    Q_INVOKABLE void refreshSnapshots();
    Q_INVOKABLE void takeSnapshot(const QString &label);
    // otherId empty: compare with the card now.
    Q_INVOKABLE void diffSnapshot(const QString &id, const QString &otherId);
    // path empty: the whole card.
    Q_INVOKABLE void planRestore(const QString &id, const QString &path);
    Q_INVOKABLE void runRestore();
    Q_INVOKABLE void deleteSnapshot(const QString &id);

    /* Captures */
    Q_INVOKABLE void readCapturesFromCard();
    Q_INVOKABLE void buildCaptureLibrary();
    Q_INVOKABLE void searchCaptures(const QString &query);
    Q_INVOKABLE void explainCapture(const QString &id);
    Q_INVOKABLE void tagCapture(const QString &id, const QStringList &add, const QStringList &remove);
    Q_INVOKABLE void findDuplicates();

    /* Sarina API (read-only, synchronous) */
    QJsonObject apiCaptureLibrary(const QString &query, int limit);
    QJsonObject apiCaptureExplain(const QString &id);

    Q_INVOKABLE void smokeFinished(bool ok, const QString &detail);
    // ARGUS_QML_SMOKE_SHOTS=<folder>: where the UI check saves screenshots ("" when unset).
    Q_INVOKABLE QString smokeShotPath(const QString &name) const;

signals:
    void busyChanged();
    void statusChanged();
    void cardChanged();
    void packageReportChanged();
    void installPlanChanged();
    void checkupReportChanged();
    void rebuildReportChanged();
    void snapshotsChanged();
    void snapshotDiffChanged();
    void restorePlanChanged();
    void capturesChanged();
    void captureDetailChanged();
    void duplicatesChanged();
    // op: package, installPlan, install, checkup, rebuild, snapshots, snapshot, diff,
    // restorePlan, restore, deleteSnapshot, readCaptures, captureIndex, captureSearch,
    // captureExplain, captureTag, duplicates
    void operationFinished(const QString &op, bool ok, const QString &message);

private:
    bool begin(const QString &op, const QString &text, bool needsCard = true);
    void end(const QString &op, bool ok, const QString &message);
    void setStatus(const QString &text, double progress = -1);
    void runTool(const QString &op, const QStringList &args, std::function<void(const QJsonObject &)> onOk,
                 bool exclusive, int timeoutMs = 0);
    void downloadAll(const QStringList &paths, const QString &staging, std::function<void(int failed)> done);
    void writeAll(const QVariantList &items, std::function<void(int written, int failed, QStringList errors)> done);
    QString apiArg() const;

    QPointer<DeviceStorage> m_storage;
    QString m_firmwareApi;
    bool m_apiFromDevice;
    bool m_busy;
    QString m_status;
    double m_progress;
    QString m_lastQuery;

    QVariantMap m_packageReport;
    QVariantList m_installPlan;
    QVariantMap m_checkupReport;
    QVariantMap m_rebuildReport;
    QVariantList m_snapshots;
    QVariantMap m_snapshotDiff;
    QVariantMap m_restorePlan;
    QVariantList m_captures;
    QVariantMap m_captureDetail;
    QVariantList m_duplicates;
};

}
