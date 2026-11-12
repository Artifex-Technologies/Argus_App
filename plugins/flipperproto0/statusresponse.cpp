#include "statusresponse.h"

StatusPingResponse::StatusPingResponse(MessageWrapper &wrapper, QObject *parent):
    MainResponse(wrapper, parent)
{}

const QByteArray StatusPingResponse::data() const
{
    // Missing data (an empty bytes field left out) is empty, never a NULL dereference.
    const auto *d = message().content.system_ping_response.data;
    return d ? QByteArray((const char*)d->bytes, d->size) : QByteArray();
}
