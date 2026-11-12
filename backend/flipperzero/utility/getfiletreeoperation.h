#pragma once

#include "abstractutilityoperation.h"
#include "fileinfo.h"

class QSerialPort;

namespace Flipper {
namespace Zero {

class GetFileTreeOperation : public AbstractUtilityOperation
{
    Q_OBJECT

    enum State {
        Running = AbstractOperation::User
    };

public:
    GetFileTreeOperation(ProtobufSession *rpc, DeviceState *deviceState, const QByteArray &rootPath, QObject *parent = nullptr);
    const QString description() const override;
    const FileInfoList &files() const;

    // Device paths of entries left out because their names are unsafe to use on the local disk
    // (see isSafeName); they are never listed or descended into.
    const QByteArrayList &refused() const;

    // True when a name from a device listing can be one local path component: not empty, not
    // "." or "..", valid UTF-8 of at most 255 bytes, no '/', '\', NUL or control character
    // (C0, DEL, C1). The same rule as flipper_rpc::transfer::safe_name in the Rust port.
    static bool isSafeName(const QByteArray &name);

private slots:
    void nextStateLogic() override;

private:
    void listDirectory(const QByteArray &path);

    QByteArray m_rootPath;
    QByteArray m_currentPath;
    FileInfoList m_result;
    QByteArrayList m_refused;
    int m_pendingCount;
};

}
}
