#pragma once

#include <QStringList>

class QCoreApplication;

/* Headless Argus features, reached as `qFlipper-cli chymaera argus-test` (the end-to-end test
 * against a fake SD card, no device needed) and `qFlipper-cli chymaera argus <command>` (the same
 * flows against a connected Flipper, or a folder with --fake-card). */
class ArgusCli
{
public:
    static int runTest(QCoreApplication &app, const QStringList &args);
    static int run(QCoreApplication &app, const QStringList &args);
};
