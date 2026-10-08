#include "chymaeratypes.h"

namespace Chymaera {

QString severityName(Severity s)
{
    switch(s) {
    case Severity::Debug:    return QStringLiteral("debug");
    case Severity::Info:     return QStringLiteral("info");
    case Severity::Notice:   return QStringLiteral("notice");
    case Severity::Warning:  return QStringLiteral("warning");
    case Severity::Error:    return QStringLiteral("error");
    case Severity::Critical: return QStringLiteral("critical");
    default:                 return QStringLiteral("info");
    }
}

QString sourceName(Source s)
{
    switch(s) {
    case Source::System:  return QStringLiteral("system");
    case Source::Device:  return QStringLiteral("device");
    case Source::Sarina:  return QStringLiteral("sarina");
    case Source::Capture: return QStringLiteral("capture");
    case Source::Export:  return QStringLiteral("export");
    case Source::User:    return QStringLiteral("user");
    default:              return QStringLiteral("user");
    }
}

Severity severityFromName(const QString &name, Severity fallback)
{
    const auto n = name.trimmed().toLower();
    if(n == QStringLiteral("debug"))    return Severity::Debug;
    if(n == QStringLiteral("info"))     return Severity::Info;
    if(n == QStringLiteral("notice"))   return Severity::Notice;
    if(n == QStringLiteral("warning"))  return Severity::Warning;
    if(n == QStringLiteral("error"))    return Severity::Error;
    if(n == QStringLiteral("critical")) return Severity::Critical;
    return fallback;
}

Source sourceFromName(const QString &name, Source fallback)
{
    const auto n = name.trimmed().toLower();
    if(n == QStringLiteral("system"))  return Source::System;
    if(n == QStringLiteral("device"))  return Source::Device;
    if(n == QStringLiteral("sarina"))  return Source::Sarina;
    if(n == QStringLiteral("capture")) return Source::Capture;
    if(n == QStringLiteral("export"))  return Source::Export;
    if(n == QStringLiteral("user"))    return Source::User;
    return fallback;
}

}
