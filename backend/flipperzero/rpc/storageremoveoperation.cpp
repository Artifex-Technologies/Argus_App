#include "storageremoveoperation.h"

#include "protobufplugininterface.h"
#include "mainresponseinterface.h"

using namespace Flipper;
using namespace Zero;

StorageRemoveOperation::StorageRemoveOperation(uint32_t id, const QByteArray &path, bool recursive, QObject *parent):
    AbstractProtobufOperation(id, parent),
    m_path(path),
    m_recursive(recursive)
{}

const QString StorageRemoveOperation::description() const
{
    return QStringLiteral("Storage Remove @%1").arg(QString(m_path));
}

const QByteArray StorageRemoveOperation::encodeRequest(ProtobufPluginInterface *encoder)
{
    return encoder->storageRemove(id(), m_path, m_recursive);
}

// Until 2026-11-12 a refused remove (a folder not empty, access denied) counted as success.
bool StorageRemoveOperation::acceptsStatus(int status) const
{
    return status == MainResponseInterface::StatusOk;
}
