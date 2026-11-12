#include "getfiletreeoperation.h"

#include "flipperzero/devicestate.h"
#include "flipperzero/protobufsession.h"
#include "flipperzero/rpc/storagelistoperation.h"

using namespace Flipper;
using namespace Zero;

GetFileTreeOperation::GetFileTreeOperation(ProtobufSession *rpc, DeviceState *deviceState, const QByteArray &rootPath, QObject *parent):
    AbstractUtilityOperation(rpc, deviceState, parent),
    m_rootPath(rootPath),
    m_pendingCount(0)
{}

const QString GetFileTreeOperation::description() const
{
    return QStringLiteral("Get File Tree @%1").arg(deviceState()->name());
}

const FileInfoList &GetFileTreeOperation::files() const
{
    return m_result;
}

const QByteArrayList &GetFileTreeOperation::refused() const
{
    return m_refused;
}

bool GetFileTreeOperation::isSafeName(const QByteArray &name)
{
    if(name.isEmpty() || name.size() > 255 || name == "." || name == "..") {
        return false;
    }

    const auto text = QString::fromUtf8(name);
    if(text.toUtf8() != name) {
        return false;
    }

    for(const auto &c : text) {
        if(c == QLatin1Char('/') || c == QLatin1Char('\\') || c.category() == QChar::Other_Control) {
            return false;
        }
    }

    return true;
}

void GetFileTreeOperation::nextStateLogic()
{
    if(operationState() == BasicOperationState::Ready) {
        setOperationState(State::Running);
        listDirectory(m_rootPath);

    } else if(operationState() == State::Running) {
        --m_pendingCount;
        auto *op = qobject_cast<StorageListOperation*>(sender());

        if(op->isError()) {
            finishWithError(op->error(), op->errorString());
            return;
        }

        for(const auto &fileInfo : qAsConst(op->files())) {
            // A name from the device that could lead outside a local folder is left out.
            if(!isSafeName(fileInfo.name) || fileInfo.absolutePath.size() > 255) {
                m_refused.append(fileInfo.absolutePath);
                continue;
            }

            if(fileInfo.type == FileType::Directory) {
                listDirectory(fileInfo.absolutePath);
            }

            m_result.push_back(fileInfo);
        }

        if(!m_pendingCount) {
            finish();
        }
    }
}

void GetFileTreeOperation::listDirectory(const QByteArray &path)
{
    ++m_pendingCount;
    auto *op = rpc()->storageList(path);
    connect(op, &AbstractOperation::finished, this, &GetFileTreeOperation::nextStateLogic);
}
