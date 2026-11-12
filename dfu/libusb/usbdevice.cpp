#include "usbdevice.h"

#include <libusb.h>
#include <QThread>

#include <cerrno>
#include <cstring>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/un.h>
#include <unistd.h>

#include "debug.h"

#define RETRY_COUNT 25
#define RETRY_INTERVAL_MS 50

/* Test hook (backend/testhooks.h; this library reads the variables itself). With ARGUS_TEST_DFU_SOCKET set, a
 * device is a connection to that Unix socket, where the Rust port's fake bootloader answers one line per request
 * (flipper_dfu::line: ALT n, OUT request value hex, IN request value length, DESC, STRING alt), and libusb is never
 * asked to open, claim or talk to anything. Only the libusb calls are replaced: the retries and checks around them
 * stay as they are. With any of the app's other test hooks set but no socket, no device opens at all. Unset (always,
 * outside tests), nothing changes. */
static bool isTestMode()
{
    return !qEnvironmentVariableIsEmpty("ARGUS_TEST_DFU_SOCKET") || !qEnvironmentVariableIsEmpty("ARGUS_TEST_DEVICE_STATE") ||
           !qEnvironmentVariableIsEmpty("ARGUS_TEST_SERIAL_PORT") || !qEnvironmentVariableIsEmpty("ARGUS_TEST_FIRMWARE_DIR");
}

struct USBDevice::USBDevicePrivate {
    libusb_device *libusbDevice = nullptr;
    libusb_device_handle *libusbDeviceHandle = nullptr;

    bool isTest = false;
    int testFd = -1;
    QByteArray testBuffer;

    bool testOpen();
    void testClose();
    // One request line and its answer line; false once the device is gone.
    bool testAsk(const QByteArray &line, QByteArray &answer);
    // A request answered with DATA: the number of bytes (copied to out, at most maxLength) or a libusb error.
    int testData(const QByteArray &line, QByteArray &out, int maxLength);
};

bool USBDevice::USBDevicePrivate::testOpen()
{
    const auto path = qgetenv("ARGUS_TEST_DFU_SOCKET");
    sockaddr_un addr;

    if(path.isEmpty() || (size_t)path.size() >= sizeof(addr.sun_path)) {
        return false;
    }

    std::memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    std::memcpy(addr.sun_path, path.constData(), path.size());

    const auto fd = ::socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if(fd < 0) {
        return false;
    }

    timeval tv = {5, 0};
    ::setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    ::setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));

    if(::connect(fd, (const sockaddr*)&addr, sizeof(addr)) != 0) {
        ::close(fd);
        return false;
    }

    testFd = fd;
    testBuffer.clear();
    return true;
}

void USBDevice::USBDevicePrivate::testClose()
{
    if(testFd >= 0) {
        ::close(testFd);
        testFd = -1;
    }

    testBuffer.clear();
}

bool USBDevice::USBDevicePrivate::testAsk(const QByteArray &line, QByteArray &answer)
{
    // the longest answer: 64 KiB as hex
    static constexpr int MAX_LINE = 256 * 1024;

    if(testFd < 0) {
        return false;
    }

    const auto out = line + '\n';
    for(qsizetype sent = 0; sent < out.size();) {
        const auto n = ::send(testFd, out.constData() + sent, out.size() - sent, MSG_NOSIGNAL);
        if(n <= 0) {
            testClose();
            return false;
        }
        sent += n;
    }

    for(;;) {
        const auto end = testBuffer.indexOf('\n');
        if(end >= 0) {
            answer = testBuffer.left(end);
            testBuffer.remove(0, end + 1);
            return true;
        } else if(testBuffer.size() > MAX_LINE) {
            testClose();
            return false;
        }

        char chunk[4096];
        const auto n = ::recv(testFd, chunk, sizeof(chunk), 0);
        if(n <= 0) {
            testClose();
            return false;
        }
        testBuffer.append(chunk, n);
    }
}

int USBDevice::USBDevicePrivate::testData(const QByteArray &line, QByteArray &out, int maxLength)
{
    QByteArray answer;

    if(!testAsk(line, answer)) {
        return LIBUSB_ERROR_NO_DEVICE;
    } else if(!answer.startsWith("DATA ")) {
        return LIBUSB_ERROR_PIPE;
    }

    const auto hex = answer.mid(5);
    const auto data = (hex == "-") ? QByteArray() : QByteArray::fromHex(hex);

    if(data.size() > maxLength) {
        return LIBUSB_ERROR_OVERFLOW;
    }

    out = data;
    return (int)data.size();
}

USBDevice::USBDevice(const USBDeviceInfo &info, QObject *parent):
    QObject(parent),
    m_p(new USBDevicePrivate)
{
    m_p->isTest = isTestMode();

    if(!m_p->isTest) {
        m_p->libusbDevice = (libusb_device*)info.backendData().value<void*>();
    }
}

USBDevice::~USBDevice()
{
    // TODO: Find out how to close it safely
//  close();
    m_p->testClose();
    delete m_p;
}

bool USBDevice::open()
{
    if(m_p->isTest) {
        if(m_p->testFd < 0 && !m_p->testOpen()) {
            error_msg("Unable to open device: the test socket does not answer");
        }
        return m_p->testFd >= 0;
    }

    if(!m_p->libusbDeviceHandle) {
        const auto err = libusb_open(m_p->libusbDevice, &(m_p->libusbDeviceHandle));
        check_continue(!err, QString("Unable to open device: %1").arg(libusb_error_name(err)));
    }

    return m_p->libusbDeviceHandle;
}

void USBDevice::close()
{
    if(m_p->isTest) {
        m_p->testClose();
        return;
    }

    if(!m_p->libusbDeviceHandle) {
        return;
    }

    libusb_close(m_p->libusbDeviceHandle);
    m_p->libusbDeviceHandle = nullptr;
}

bool USBDevice::claimInterface(int interfaceNum)
{
    int retryCount = RETRY_COUNT, err;

    do {
        err = m_p->isTest ? (m_p->testFd >= 0 ? 0 : LIBUSB_ERROR_NO_DEVICE) : libusb_claim_interface(m_p->libusbDeviceHandle, interfaceNum);
        if(!err) { break; }
        QThread::msleep(RETRY_INTERVAL_MS);
    } while(--retryCount);

    check_continue(!err, QString("Failed to claim interface: %1").arg(libusb_error_name(err)));
    return !err;
}

bool USBDevice::releaseInterface(int interfaceNum)
{
    const auto err = m_p->isTest ? (m_p->testFd >= 0 ? 0 : LIBUSB_ERROR_NO_DEVICE) : libusb_release_interface(m_p->libusbDeviceHandle, interfaceNum);
    check_continue(!err, QString("Failed to release interface: %1").arg(libusb_error_name(err)));
    return !err;
}

bool USBDevice::setInterfaceAltSetting(int interfaceNum, uint8_t alt)
{
    int retryCount = RETRY_COUNT, err;

    do {
        if(m_p->isTest) {
            QByteArray answer;
            err = !m_p->testAsk("ALT " + QByteArray::number(alt), answer) ? LIBUSB_ERROR_NO_DEVICE : (answer == "OK" ? 0 : LIBUSB_ERROR_PIPE);
        } else {
            err = libusb_set_interface_alt_setting(m_p->libusbDeviceHandle, interfaceNum, alt);
        }
        if(!err) { break; }
        QThread::msleep(RETRY_INTERVAL_MS);
    } while(--retryCount);

    check_continue(!err, QString("Failed to set alternate setting: %1").arg(libusb_error_name(err)));
    return !err;
}

bool USBDevice::controlTransfer(uint8_t requestType, uint8_t request, uint16_t value, uint16_t index, const QByteArray &buf)
{
    int retryCount = RETRY_COUNT, res;

    do {
        auto *data = buf.isEmpty() ? nullptr : (unsigned char*)(buf.data());
        if(m_p->isTest) {
            QByteArray answer;
            const auto line = "OUT " + QByteArray::number(request) + ' ' + QByteArray::number(value) + ' ' + (buf.isEmpty() ? QByteArray("-") : buf.toHex());
            if(!m_p->testAsk(line, answer)) {
                res = LIBUSB_ERROR_NO_DEVICE;
            } else {
                bool ok = false;
                const auto n = answer.startsWith("OK ") ? answer.mid(3).toInt(&ok) : -1;
                res = (ok && n >= 0) ? n : LIBUSB_ERROR_PIPE;
            }
        } else {
            res = libusb_control_transfer(m_p->libusbDeviceHandle, requestType, request, value, index, data, buf.size(), m_timeout);
        }
        if(res >= 0) { break; }
        QThread::msleep(RETRY_INTERVAL_MS);
    } while(--retryCount);

    if(res < 0) {
        error_msg(QString("Failed to perform control transfer: %1").arg(libusb_error_name(res)));
    } else if(res != buf.size()) {
        debug_msg("Requested and transferred data size differ");
    } else {}

    return res == buf.size();
}

QByteArray USBDevice::controlTransfer(uint8_t requestType, uint8_t request, uint16_t value, uint16_t index, uint16_t length)
{
    QByteArray buf(length, 0);
    int retryCount = RETRY_COUNT, res;

    do {
        if(m_p->isTest) {
            QByteArray data;
            res = m_p->testData("IN " + QByteArray::number(request) + ' ' + QByteArray::number(value) + ' ' + QByteArray::number(length), data, length);
            if(res >= 0) {
                std::memcpy(buf.data(), data.constData(), res);
            }
        } else {
            res = libusb_control_transfer(m_p->libusbDeviceHandle, requestType, request, value, index, (unsigned char*)(buf.data()), length, m_timeout);
        }
        if(res >= 0) { break; }
        QThread::msleep(RETRY_INTERVAL_MS);
    } while ((res < 0) && --retryCount);

    if(res < 0) {
        buf.clear();
        error_msg(QString("Failed to perform control transfer: %1").arg(libusb_error_name(res)));
    } else if(res != length) {
        buf.resize(res);
        debug_msg("Requested and transferred data size differ");
    } else {}

    return buf;
}

QByteArray USBDevice::extraInterfaceDescriptor(int interfaceNum, uint8_t type, int length)
{
    QByteArray ret;
    libusb_config_descriptor *cfg;

    if(m_p->isTest) {
        // the same check as below on the one descriptor the fake has
        Q_UNUSED(interfaceNum)
        QByteArray data;
        if(m_p->testData("DESC", data, 255) == length && length >= 2 && (uint8_t)data.at(1) == type) {
            ret = data;
        }
        return ret;
    }

    check_return_val(!libusb_get_config_descriptor(m_p->libusbDevice, 0, &cfg), "Failed to get configuration descriptor", ret);

    const auto intf = cfg->interface[interfaceNum];

    for(auto i = 0; i < intf.num_altsetting; ++i) {
        const auto altintf = intf.altsetting[i];
        if((altintf.extra_length == length) && (altintf.extra[1] == type)) {
            ret.append((const char*)(altintf.extra), altintf.extra_length);
            break;
        }
    }

    libusb_free_config_descriptor(cfg);

    return ret;
}

QByteArray USBDevice::stringInterfaceDescriptor(uint8_t alt)
{
    libusb_config_descriptor *cfg;

    if(m_p->isTest) {
        QByteArray data;
        if(m_p->testData("STRING " + QByteArray::number(alt), data, 254) < 0) {
            error_msg("Failed to get string descriptor");
            data.clear();
        }
        return data;
    }

    check_return_val(!libusb_get_config_descriptor(m_p->libusbDevice, 0, &cfg), "Failed to get configuration descriptor", QByteArray());

    const auto BUF_SIZE = 254;
    QByteArray buf(BUF_SIZE, 0);


    const auto intf = *(cfg->interface);
    const auto res = libusb_get_string_descriptor_ascii(m_p->libusbDeviceHandle, intf.altsetting[alt].iInterface, (unsigned char*)buf.data(), BUF_SIZE);

    if(res < 0) {
        error_msg(QString("Failed to get string descriptor: %1").arg(libusb_error_name(res)));
        buf.clear();

    } else {
        buf.resize(res);
    }

    libusb_free_config_descriptor(cfg);

    return buf;
}
