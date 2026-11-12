#ifndef DFUDEVICE_H
#define DFUDEVICE_H

#include <QIODevice>
#include <QMetaEnum>

#include "usbdevice.h"
#include "dfusefile.h"

// NOTE: This class should reveal about USB internals as little as possible.
// TODO: Separate STM32 protocol and standard protocol into respective classes.

class DfuseDevice : public USBDevice
{
    Q_OBJECT

    struct StatusType {
        enum Status {
            OK = 0,
            ERR_TARGET,
            ERR_FILE,
            ERR_WRITE,
            ERR_ERASE,
            ERR_CHECK_ERASED,
            ERR_PROG,
            ERR_VERIFY,
            ERR_ADDRESS,
            ERR_NOTDONE,
            ERR_FIRMWARE,
            ERR_VENDOR,
            ERR_USBR,
            ERR_POR,
            ERR_UNKNOWN,
            ERR_STALLEDPKT,

            ERR_UNDEFINED = 0xff
        };

        enum State {
            APP_IDLE = 0,
            APP_DETACH,
            DFU_IDLE,
            DFU_DNLOAD_SYNC,
            DFU_DNBUSY,
            DFU_DNLOAD_IDLE,
            DFU_MANIFEST_SYNC,
            DFU_MANIFEST,
            DFU_MANIFEST_WAIT_RESET,
            DFU_UPLOAD_IDLE,
            DFU_ERROR,

            STATE_UNDEFINED = 0xff
        };

        Status bStatus = ERR_UNDEFINED;
        State bState = STATE_UNDEFINED;

        uint32_t bwPollTimeout = 0;
        uint8_t iString = 0;

        // False when DFU_GETSTATUS failed or did not answer 6 bytes
        bool isValid = false;

        // The wait the device asked for, at most MaxPollTimeout
        unsigned long waitTime() const;
    };

    enum Request {
        DFU_DETACH = 0,
        DFU_DNLOAD,
        DFU_UPLOAD,
        DFU_GETSTATUS,
        DFU_CLRSTATUS,
        DFU_GETSTATE,
        DFU_ABORT
    };

public:
    // Every wait is bounded, the same as in the Rust port (argus-tools/crates/flipper-dfu, src/device.rs):
    // a poll timeout over MaxPollTimeout ms is clamped, and one step gives up after MaxPolls status reads.
    static const uint32_t MaxPollTimeout = 10000;
    static const int MaxPolls = 1000;
    static const size_t MaxUpload = 16 * 1024 * 1024;

    enum Operation {
        Erase,
        Download,
        Upload
    };

    DfuseDevice(const USBDeviceInfo &info, QObject *parent = nullptr);

    bool beginTransaction();
    bool endTransaction();

    uint32_t partitionOrigin(uint8_t alt = 0);

    bool erase(uint32_t addr, size_t maxSize);
    bool download(DfuseFile *file);
    bool download(QIODevice *file, uint32_t addr, uint8_t alt = 0);
    bool download(const QByteArray &data);
    bool upload(QIODevice *file, uint32_t addr, size_t maxSize, uint8_t alt = 0);
    bool leave();

signals:
    void progressChanged(const int, const double);

private:
    bool abort();

    bool clearStatus();
    StatusType getStatus();

    bool prepare();
    bool pollWhileBusy(const char *failMsg);
    bool pollUntil(StatusType::State state, const char *failMsg);
    bool setAddressPointer(uint32_t addr);
    bool erasePage(uint32_t addr);
};


#endif // DFUDEVICE_H
