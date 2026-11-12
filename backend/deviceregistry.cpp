#include "deviceregistry.h"

#include <QDebug>
#include <QMetaObject>
#include <QTimer>
#include <QFile>
#include <QLoggingCategory>

#include "flipperzero/helper/deviceinfohelper.h"
#include "flipperzero/flipperzero.h"
#include "flipperzero/devicestate.h"
#include "flipperzero/protobufsession.h"

#include "usbdevice.h"
#include "testhooks.h"

#define FLIPPER_ZERO_VID 0x0483
#define FLIPPER_ZERO_PID_VCP 0x5740
#define FLIPPER_ZERO_PID_DFU 0xdf11

Q_LOGGING_CATEGORY(LOG_DEVREG, "REG");

using namespace Flipper;

DeviceRegistry::DeviceRegistry(QObject *parent):
    QObject(parent),
    m_detector(new USBDeviceDetector(this)),
    m_error(BackendError::UnknownError),
    m_isQueryInProgress(false)
{
    if(!Argus::testDeviceStateFile().isEmpty()) {
        // Test hook (testhooks.h): no USB watching at all; the harness's state file says what is plugged in.
        qCDebug(LOG_DEVREG).noquote() << "Test device state file:" << Argus::testDeviceStateFile();
        auto *timer = new QTimer(this);
        connect(timer, &QTimer::timeout, this, &DeviceRegistry::pollTestDeviceState);
        timer->start(50);
        return;

    } else if(Argus::isTestDevice()) {
        // Test hook (testhooks.h): no USB watching at all; one device on the given port (none without one).
        if(!Argus::isTestSerialPort()) {
            return;
        }

        qCDebug(LOG_DEVREG).noquote() << "Test serial port:" << Argus::testSerialPort();
        const auto testDevice = USBDeviceInfo(FLIPPER_ZERO_VID, FLIPPER_ZERO_PID_VCP)
            .withManufacturer(QStringLiteral("Flipper Devices Inc."))
            .withProductDescription(QStringLiteral("Flipper Control Virtual ComPort"))
            .withSerialNumber(QStringLiteral("argus-test"))
            .withBackendData(QStringLiteral("argus-test"));
        QTimer::singleShot(0, this, [=]() { insertDevice(testDevice); });
        return;
    }

    connect(m_detector, &USBDeviceDetector::devicePluggedIn, this, &DeviceRegistry::insertDevice);
    connect(m_detector, &USBDeviceDetector::deviceUnplugged, this, &DeviceRegistry::removeDevice);

    m_detector->setWantedDevices({
        USBDeviceInfo(FLIPPER_ZERO_VID, FLIPPER_ZERO_PID_DFU),
        USBDeviceInfo(FLIPPER_ZERO_VID, FLIPPER_ZERO_PID_VCP)
            .withManufacturer("Flipper Devices Inc.")
            .withProductDescription("Flipper Control Virtual ComPort")
                                 });
}

// Test hook (testhooks.h): the devices the harness's state file describes. They have no libusb device behind
// them: the bootloader is reached over ARGUS_TEST_DFU_SOCKET, the firmware over ARGUS_TEST_SERIAL_PORT.
static USBDeviceInfo testDeviceInfo(const QString &state)
{
    if(state == QStringLiteral("dfu")) {
        return USBDeviceInfo(FLIPPER_ZERO_VID, FLIPPER_ZERO_PID_DFU)
            .withManufacturer(QStringLiteral("STMicroelectronics"))
            .withProductDescription(QStringLiteral("DFU in FS Mode"))
            .withSerialNumber(QStringLiteral("argus-test-dfu"))
            .withBackendData(QStringLiteral("argus-test-dfu"));
    } else if(state == QStringLiteral("serial")) {
        return USBDeviceInfo(FLIPPER_ZERO_VID, FLIPPER_ZERO_PID_VCP)
            .withManufacturer(QStringLiteral("Flipper Devices Inc."))
            .withProductDescription(QStringLiteral("Flipper Control Virtual ComPort"))
            .withSerialNumber(QStringLiteral("argus-test"))
            .withBackendData(QStringLiteral("argus-test"));
    } else {
        return USBDeviceInfo();
    }
}

void DeviceRegistry::pollTestDeviceState()
{
    QFile file(Argus::testDeviceStateFile());
    QString line;

    if(file.open(QIODevice::ReadOnly)) {
        line = QString::fromLatin1(file.read(64)).trimmed();
    }

    // "KIND N": N counts the device's lives, so a reset and return between two polls is still seen as one
    // (USB hot-plug queues both events too)
    if(line == m_testDeviceState) {
        return;
    }

    auto kind = line.section(' ', 0, 0);

    // a state without its transport is no device: nothing may fall through to libusb or a real port
    if((kind == QStringLiteral("dfu") && Argus::testDfuSocket().isEmpty()) ||
       (kind == QStringLiteral("serial") && !Argus::isTestSerialPort()) ||
       (kind != QStringLiteral("dfu") && kind != QStringLiteral("serial"))) {
        kind = QStringLiteral("none");
    }

    qCDebug(LOG_DEVREG).noquote() << "Test device state:" << m_testDeviceState << "->" << line;

    const auto oldKind = m_testDeviceState.section(' ', 0, 0);
    if(oldKind == QStringLiteral("serial")) {
        // A Flipper that leaves USB takes its serial port with it, and the session sees the connection lost and
        // closes the port; a pseudo-terminal stays, so the session is told here (else the port stays locked).
        const auto info = testDeviceInfo(oldKind);
        for(auto *device : qAsConst(m_devices)) {
            if(device->deviceState()->deviceInfo().usbInfo.backendData() == info.backendData()) {
                device->rpc()->stopSession();
            }
        }
    }
    if(oldKind == QStringLiteral("dfu") || oldKind == QStringLiteral("serial")) {
        removeDevice(testDeviceInfo(oldKind));
    }

    m_testDeviceState = (kind == QStringLiteral("none")) ? QStringLiteral("none") : line;

    if(kind != QStringLiteral("none")) {
        insertDevice(testDeviceInfo(kind));
    }
}

void DeviceRegistry::setBackendLogLevel(int logLevel)
{
    m_detector->setLogLevel(logLevel);
}

FlipperZero *DeviceRegistry::currentDevice() const
{
    return m_devices.isEmpty() ? nullptr : m_devices.first();
}

int DeviceRegistry::deviceCount() const
{
    return m_devices.size();
}

BackendError::ErrorType DeviceRegistry::error() const
{
    return m_error;
}

void DeviceRegistry::clearError()
{
    setError(BackendError::UnknownError);
}

bool DeviceRegistry::isQueryInProgress() const
{
    return m_isQueryInProgress;
}

void DeviceRegistry::insertDevice(const USBDeviceInfo &info)
{
    if(!info.isComplete()) {
        qCDebug(LOG_DEVREG).noquote().nospace()
            << "Incomplete device info: VID_0x" << QString::number(info.vendorID(), 16) << ":PID_0x"
            << QString::number(info.productID(), 16);

        setError(BackendError::InvalidDevice);

    } else if(info.vendorID() != FLIPPER_ZERO_VID) {
        qCDebug(LOG_DEVREG) << "Unexpected device VID and PID";
        setError(BackendError::InvalidDevice);

    } else {
        setQueryInProgress(true);
        qCDebug(LOG_DEVREG).noquote().nospace()
            << "Detected new device: VID_0x" << QString::number(info.vendorID(), 16) << ":PID_0x" << QString::number(info.productID(), 16);

        auto *fetcher = Zero::AbstractDeviceInfoHelper::create(info, this);
        connect(fetcher, &Zero::AbstractDeviceInfoHelper::finished, this, &DeviceRegistry::processDevice);
        connect(fetcher, &Zero::AbstractDeviceInfoHelper::finished, fetcher, &QObject::deleteLater);
    }
}

void DeviceRegistry::removeDevice(const USBDeviceInfo &info)
{
    const auto it = std::find_if(m_devices.begin(), m_devices.end(), [&](Flipper::FlipperZero *dev) {
        const auto &deviceInfo = dev->deviceState()->deviceInfo().usbInfo;
        return deviceInfo.backendData() == info.backendData();
    });

    if(it != m_devices.end()) {
        const auto idx = std::distance(m_devices.begin(), it);
        auto *device = m_devices.at(idx);

        if(!device->deviceState()->isPersistent()) {
            qCDebug(LOG_DEVREG).noquote().nospace()
                << "Device disconnected: VID_0x" << QString::number(info.vendorID(), 16) << ":PID_0x" << QString::number(info.productID(), 16);

            m_devices.takeAt(idx)->deleteLater();
            emit deviceCountChanged();
            emit currentDeviceChanged();

        } else {
            qCDebug(LOG_DEVREG).noquote().nospace()
                << "Device went offline: VID_0x" << QString::number(info.vendorID(), 16) << ":PID_0x" << QString::number(info.productID(), 16);

            device->deviceState()->setOnline(false);
        }
    }
}

void DeviceRegistry::removeOfflineDevices()
{
    auto it = std::remove_if(m_devices.begin(), m_devices.end(), [](Flipper::FlipperZero *arg) {
        return !arg->deviceState()->isOnline();
    });

    for(const auto end = m_devices.end(); it != end; ++it) {
        qCDebug(LOG_DEVREG).noquote() << "Removed offline device:" << (*it)->deviceState()->name();

        m_devices.erase(it);
        emit deviceCountChanged();

        (*it)->deleteLater();
    }
}

void DeviceRegistry::processDevice()
{
    setQueryInProgress(false);

    auto *fetcher = qobject_cast<Zero::AbstractDeviceInfoHelper*>(sender());
    const auto &info = fetcher->result();

    if(fetcher->isError()) {
        qCDebug(LOG_DEVREG).noquote() << "Device initialization failed:" << fetcher->errorString();
        setError(fetcher->error());
        return;
    }

    const auto it = std::find_if(m_devices.begin(), m_devices.end(), [&info](Flipper::FlipperZero *arg) {
        return info.name == arg->deviceState()->name();
    });

    if(it != m_devices.end()) {
        // Preserving the old instance
        qCDebug(LOG_DEVREG) << "Device went back online";
        (*it)->deviceState()->setDeviceInfo(info);

    } else {
        qCDebug(LOG_DEVREG) << "Registering the device";

        auto *device = new FlipperZero(info, this);
        m_devices.append(device);

        emit deviceCountChanged();

        if(m_devices.size() == 1) {
            emit currentDeviceChanged();
        }
    }
}

void DeviceRegistry::setError(BackendError::ErrorType newError)
{
    if(m_error == newError) {
        return;
    }

    m_error = newError;
    emit errorOccured();
}

void DeviceRegistry::setQueryInProgress(bool set)
{
    if(m_isQueryInProgress == set) {
        return;
    }

    m_isQueryInProgress = set;
    emit isQueryInProgressChanged();
}
