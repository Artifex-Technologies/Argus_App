// A stand-in for the C++ app's libusb USBDevice (dfu/libusb/usbdevice.h, the same class and header)
// that opens nothing: every request is written to stdout as one line and its answer read from stdin,
// where the Rust test (tests/qt_device.rs) runs its fake DFU device. So the C++ DfuseDevice and the
// Rust port are driven by the same fake, request for request. QThread::msleep is replaced too, so a
// device asking for a long wait is recorded, not waited for.
#include "libusb/usbdevice.h"

#include <QThread>

#include <cstdio>
#include <cstdlib>
#include <iostream>
#include <string>

struct USBDevice::USBDevicePrivate {};

static QByteArray hexOrDash(const QByteArray &b)
{
    return b.isEmpty() ? QByteArray("-") : b.toHex();
}

// Sends one request line and returns the answer line (exits if the other side has gone).
static QByteArray ask(const QByteArray &line)
{
    std::cout << line.toStdString() << std::endl;
    std::string answer;
    if(!std::getline(std::cin, answer)) {
        std::exit(3);
    }
    return QByteArray::fromStdString(answer);
}

static QByteArray data(const QByteArray &answer)
{
    if(!answer.startsWith("DATA ")) {
        return QByteArray();
    }
    const auto hex = answer.mid(5);
    return hex == "-" ? QByteArray() : QByteArray::fromHex(hex);
}

void QThread::msleep(unsigned long msecs)
{
    std::cout << "SLEEP " << msecs << std::endl;
}

USBDevice::USBDevice(const USBDeviceInfo &info, QObject *parent):
    QObject(parent),
    m_p(new USBDevicePrivate)
{
    Q_UNUSED(info)
}

USBDevice::~USBDevice()
{
    delete m_p;
}

bool USBDevice::open()
{
    return true;
}

void USBDevice::close()
{}

bool USBDevice::claimInterface(int interfaceNum)
{
    Q_UNUSED(interfaceNum)
    return true;
}

bool USBDevice::releaseInterface(int interfaceNum)
{
    Q_UNUSED(interfaceNum)
    return true;
}

bool USBDevice::setInterfaceAltSetting(int interfaceNum, uint8_t alt)
{
    Q_UNUSED(interfaceNum)
    return ask("ALT " + QByteArray::number(alt)) == "OK";
}

bool USBDevice::controlTransfer(uint8_t requestType, uint8_t request, uint16_t value, uint16_t index, const QByteArray &buf)
{
    Q_UNUSED(requestType)
    Q_UNUSED(index)
    const auto answer = ask("OUT " + QByteArray::number(request) + ' ' + QByteArray::number(value) + ' ' + hexOrDash(buf));
    return answer.startsWith("OK ") && answer.mid(3).toLongLong() == buf.size();
}

QByteArray USBDevice::controlTransfer(uint8_t requestType, uint8_t request, uint16_t value, uint16_t index, uint16_t length)
{
    Q_UNUSED(requestType)
    Q_UNUSED(index)
    return data(ask("IN " + QByteArray::number(request) + ' ' + QByteArray::number(value) + ' ' + QByteArray::number(length)));
}

QByteArray USBDevice::extraInterfaceDescriptor(int interfaceNum, uint8_t type, int length)
{
    Q_UNUSED(interfaceNum)
    Q_UNUSED(type)
    Q_UNUSED(length)
    return data(ask("DESC"));
}

QByteArray USBDevice::stringInterfaceDescriptor(uint8_t alt)
{
    return data(ask("STRING " + QByteArray::number(alt)));
}
