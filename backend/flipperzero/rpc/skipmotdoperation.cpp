#include "skipmotdoperation.h"

#include <QSerialPort>

#include "testhooks.h"

using namespace Flipper;
using namespace Zero;

SkipMOTDOperation::SkipMOTDOperation(QSerialPort *serialPort, QObject *parent):
    SimpleSerialOperation(serialPort, parent)
{}

const QString SkipMOTDOperation::description() const
{
    return QStringLiteral("Skip MOTD @%1").arg(QString(serialPort()->portName()));
}

QByteArray SkipMOTDOperation::endOfMessageToken() const
{
    return QByteArrayLiteral("\r\n\r\n>: ");
}

uint32_t SkipMOTDOperation::flags() const
{
    // Test hook (testhooks.h): a pseudo-terminal has no DTR line.
    if(Argus::isTestSerialPort()) {
        return 0;
    }

    return DTR;
}
