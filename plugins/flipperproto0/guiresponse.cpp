#include "guiresponse.h"

GuiScreenFrameResponse::GuiScreenFrameResponse(MessageWrapper &wrapper, QObject *parent):
    MainResponse(wrapper, parent)
{}

const QByteArray GuiScreenFrameResponse::screenFrame() const
{
    // A frame without its data field (valid protobuf; the firmware never sends one) is empty.
    const auto *f = message().content.gui_screen_frame.data;
    return f ? QByteArray((const char*)f->bytes, f->size) : QByteArray();
}

Qt::ScreenOrientation GuiScreenFrameResponse::screenOrientation() const
{
    switch(message().content.gui_screen_frame.orientation) {
    case PB_Gui_ScreenOrientation_HORIZONTAL_FLIP:
        return Qt::InvertedLandscapeOrientation;
    case PB_Gui_ScreenOrientation_VERTICAL:
        return Qt::PortraitOrientation;
    case PB_Gui_ScreenOrientation_VERTICAL_FLIP:
        return Qt::InvertedPortraitOrientation;
    case PB_Gui_ScreenOrientation_HORIZONTAL:
    default:
        return Qt::LandscapeOrientation;
    }
}
