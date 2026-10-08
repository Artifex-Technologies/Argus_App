#pragma once

#include <QString>
#include <QStringList>

class QCoreApplication;

/* Headless entry point for the Chymaera subsystem, reachable as:
 *
 *   qFlipper-cli chymaera <subcommand> [args]
 *
 * It is intentionally independent of the device-oriented Cli class: none of
 * these subcommands need a connected Flipper, so they must not block waiting for
 * one. This is the surface that lets the whole subsystem (datastore, event log,
 * exporters, Sarina control API) be exercised and verified without the GUI or a
 * display server — handy when building/running under WSL. */

class ChymaeraCli
{
public:
    // sub = the arguments after "chymaera". Returns a process exit code.
    // For "serve" this runs the application event loop until interrupted.
    static int run(QCoreApplication &app, const QStringList &sub);

private:
    static void printUsage();

    static int cmdSelfTest();
    static int cmdServe(QCoreApplication &app, const QStringList &args);
    static int cmdImport(const QStringList &args);
    static int cmdParse(const QStringList &args);

    // Device-backed commands — these reach a connected Flipper via
    // ApplicationBackend and therefore run the event loop.
    static int cmdWatch(QCoreApplication &app, const QStringList &args);
    static int cmdPull(QCoreApplication &app, const QStringList &args);
};
