#include "storagemkdiroperation.h"

#include "protobufplugininterface.h"
#include "mainresponseinterface.h"

using namespace Flipper;
using namespace Zero;

StorageMkdirOperation::StorageMkdirOperation(uint32_t id, const QByteArray &path, QObject *parent):
    AbstractStorageOperation(id, path, parent)
{}

const QString StorageMkdirOperation::description() const
{
    return QStringLiteral("Storage MkDir @%1").arg(QString(path()));
}

const QByteArray StorageMkdirOperation::encodeRequest(ProtobufPluginInterface *encoder)
{
    return encoder->storageMkDir(id(), path());
}

// A folder that is already there is fine (callers make folders that may exist); any other
// refusal is an error. Until 2026-11-12 every storage error counted as success here.
bool StorageMkdirOperation::acceptsStatus(int status) const
{
    return status == MainResponseInterface::StatusOk || status == MainResponseInterface::StatusStorageExist;
}
