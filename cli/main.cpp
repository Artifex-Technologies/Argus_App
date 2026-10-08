#include "cli.h"
#include "chymaeracli.h"

#include <QSettings>

int main(int argc, char *argv[])
{
    QSettings::setDefaultFormat(QSettings::IniFormat);

    QCoreApplication::setApplicationName(QStringLiteral("%1-cli").arg(APP_NAME));
    QCoreApplication::setApplicationVersion(APP_VERSION);
    QCoreApplication::setOrganizationName(QStringLiteral("Flipper Devices Inc"));
    QCoreApplication::setOrganizationDomain(QStringLiteral("flipperdevices.com"));

    // Headless Chymaera subsystem entry point — handled before the device CLI so
    // it never blocks waiting for a Flipper:  qFlipper-cli chymaera <command>
    if(argc > 1 && QString::fromLocal8Bit(argv[1]) == QStringLiteral("chymaera")) {
        QCoreApplication app(argc, argv);
        QStringList sub;
        for(int i = 2; i < argc; ++i) {
            sub << QString::fromLocal8Bit(argv[i]);
        }
        return ChymaeraCli::run(app, sub);
    }

    Cli a(argc, argv);
    return a.exec();
}
