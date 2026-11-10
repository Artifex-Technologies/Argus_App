#include "argustools.h"

#include <QDir>
#include <QFile>
#include <QTimer>
#include <QDateTime>
#include <QFileInfo>
#include <QJsonDocument>
#include <QStandardPaths>
#include <QCoreApplication>

using namespace Argus;

static QJsonObject failure(const QString &code, const QString &message)
{
    return QJsonObject {
        {QStringLiteral("ok"), false},
        {QStringLiteral("error"), QJsonObject { {QStringLiteral("code"), code}, {QStringLiteral("message"), message} }}
    };
}

static QJsonObject parseOutput(const QByteArray &out)
{
    // the last non-empty line is the result
    const auto lines = out.trimmed().split('\n');
    if(lines.isEmpty() || lines.last().trimmed().isEmpty()) {
        return failure(QStringLiteral("no_output"), QStringLiteral("argus-tools printed nothing"));
    }
    QJsonParseError err;
    const auto doc = QJsonDocument::fromJson(lines.last(), &err);
    if(err.error != QJsonParseError::NoError || !doc.isObject()) {
        return failure(QStringLiteral("bad_output"), QStringLiteral("argus-tools printed something that is not JSON"));
    }
    return doc.object();
}

QString ArgusTools::program()
{
    const auto env = qEnvironmentVariable("ARGUS_TOOLS");
    if(!env.isEmpty() && QFileInfo(env).isExecutable()) {
        return env;
    }
    const auto local = QDir(QCoreApplication::applicationDirPath()).filePath(QStringLiteral("argus-tools"));
    if(QFileInfo(local).isExecutable()) {
        return local;
    }
    const auto onPath = QStandardPaths::findExecutable(QStringLiteral("argus-tools"));
    if(!onPath.isEmpty()) {
        return onPath;
    }
    // a shadow build in <repo>/build: the crate is built in <repo>/argus-tools/target/release
    const auto dev = QDir(QCoreApplication::applicationDirPath()).filePath(QStringLiteral("../argus-tools/target/release/argus-tools"));
    if(QFileInfo(dev).isExecutable()) {
        return QDir::cleanPath(dev);
    }
    return QString();
}

bool ArgusTools::isAvailable()
{
    return !program().isEmpty();
}

QString ArgusTools::dataDir()
{
    const auto base = QStandardPaths::writableLocation(QStandardPaths::GenericDataLocation);
    return QDir(base).filePath(QStringLiteral("argus"));
}

QString ArgusTools::snapshotsDir()
{
    return QDir(dataDir()).filePath(QStringLiteral("snapshots"));
}

QString ArgusTools::captureLibraryDir()
{
    return QDir(dataDir()).filePath(QStringLiteral("captures"));
}

QString ArgusTools::appsDir()
{
    return QDir(dataDir()).filePath(QStringLiteral("apps"));
}

QString ArgusTools::newStagingDir(const QString &tag)
{
    const auto base = QDir(dataDir()).filePath(QStringLiteral("staging"));
    QDir().mkpath(base);
    QFile::setPermissions(dataDir(), QFileDevice::ReadOwner | QFileDevice::WriteOwner | QFileDevice::ExeOwner);
    QFile::setPermissions(base, QFileDevice::ReadOwner | QFileDevice::WriteOwner | QFileDevice::ExeOwner);
    const auto stamp = QDateTime::currentDateTime().toString(QStringLiteral("yyyyMMdd-hhmmss-zzz"));
    const auto dir = QDir(base).filePath(QStringLiteral("%1-%2").arg(tag, stamp));
    QDir().mkpath(dir);
    QFile::setPermissions(dir, QFileDevice::ReadOwner | QFileDevice::WriteOwner | QFileDevice::ExeOwner);
    return dir;
}

QJsonObject ArgusTools::runSync(const QStringList &args, int timeoutMs)
{
    const auto prog = program();
    if(prog.isEmpty()) {
        return failure(QStringLiteral("no_tools"), QStringLiteral("argus-tools is not installed (build it with cargo in argus-tools/)"));
    }
    QProcess p;
    p.setProgram(prog);
    p.setArguments(args);
    p.setProcessChannelMode(QProcess::SeparateChannels);
    p.start();
    if(!p.waitForStarted(5000)) {
        return failure(QStringLiteral("start_failed"), p.errorString());
    }
    if(!p.waitForFinished(timeoutMs)) {
        p.kill();
        p.waitForFinished(1000);
        return failure(QStringLiteral("timeout"), QStringLiteral("argus-tools did not answer in time"));
    }
    return parseOutput(p.readAllStandardOutput());
}

QString ArgusTools::errorText(const QJsonObject &result)
{
    const auto e = result.value(QStringLiteral("error")).toObject();
    const auto message = e.value(QStringLiteral("message")).toString();
    return message.isEmpty() ? e.value(QStringLiteral("code")).toString() : message;
}

ArgusToolsJob::ArgusToolsJob(const QStringList &args, QObject *parent):
    QObject(parent),
    m_args(args),
    m_done(false)
{
    connect(&m_process, &QProcess::readyReadStandardError, this, [this]() {
        m_stderrBuffer += m_process.readAllStandardError();
        int nl;
        while((nl = m_stderrBuffer.indexOf('\n')) >= 0) {
            const auto line = m_stderrBuffer.left(nl);
            m_stderrBuffer.remove(0, nl + 1);
            const auto doc = QJsonDocument::fromJson(line);
            const auto text = doc.object().value(QStringLiteral("progress")).toString();
            if(!text.isEmpty()) {
                emit progress(text);
            }
        }
        if(m_stderrBuffer.size() > 1 << 20) {
            m_stderrBuffer.clear();
        }
    });
    connect(&m_process, &QProcess::finished, this, [this]() {
        finish(parseOutput(m_process.readAllStandardOutput()));
    });
    connect(&m_process, &QProcess::errorOccurred, this, [this](QProcess::ProcessError e) {
        if(e == QProcess::FailedToStart) {
            finish(failure(QStringLiteral("start_failed"), m_process.errorString()));
        }
    });
}

void ArgusToolsJob::start(int timeoutMs)
{
    const auto prog = ArgusTools::program();
    if(prog.isEmpty()) {
        QTimer::singleShot(0, this, [this]() {
            finish(failure(QStringLiteral("no_tools"), QStringLiteral("argus-tools is not installed (build it with cargo in argus-tools/)")));
        });
        return;
    }
    m_process.setProgram(prog);
    m_process.setArguments(m_args);
    m_process.start();
    if(timeoutMs > 0) {
        QTimer::singleShot(timeoutMs, this, [this]() {
            if(!m_done) {
                m_process.kill();
                finish(failure(QStringLiteral("timeout"), QStringLiteral("argus-tools did not finish in time")));
            }
        });
    }
}

void ArgusToolsJob::cancel()
{
    if(!m_done) {
        m_process.kill();
        finish(failure(QStringLiteral("cancelled"), QStringLiteral("cancelled")));
    }
}

void ArgusToolsJob::finish(const QJsonObject &result)
{
    if(m_done) {
        return;
    }
    m_done = true;
    emit finished(result);
    deleteLater();
}
