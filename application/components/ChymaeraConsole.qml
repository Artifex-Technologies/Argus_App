import QtQuick 2.15
import QtQuick.Layouts 1.15
import QtQuick.Controls 2.15

import Theme 1.0
import QFlipper 1.0

/* Chymaera operator console (Roadmap Phase 1).
 *
 * A self-contained panel that shows the live event/console stream and exposes
 * the session + Sarina control-API controls. Bind it anywhere; it talks only to
 * the Chymaera QML singleton. */
Rectangle {
    id: root

    implicitWidth: 640
    implicitHeight: 460

    color: Theme.color.darkorange1
    border.color: Theme.color.mediumorange3
    border.width: 1
    radius: 6

    function severityColor(name) {
        switch(name) {
        case "critical":
        case "error":   return Theme.color.lightred2;
        case "warning": return Theme.color.lightorange1;
        case "notice":  return Theme.color.lightgreen;
        case "debug":   return Theme.color.mediumorange1;
        default:        return Theme.color.lightorange2; // info
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 12
        spacing: 10

        // --- header / status -------------------------------------------------
        RowLayout {
            Layout.fillWidth: true
            spacing: 10

            Text {
                text: qsTr("Chymaera Console")
                color: Theme.color.lightorange2
                font.pixelSize: 18
                font.family: "Share Tech Mono"
                Layout.fillWidth: true
            }

            Rectangle {
                width: 10; height: 10; radius: 5
                color: Chymaera.serverRunning ? Theme.color.lightgreen : Theme.color.mediumorange1
            }
            Text {
                color: Theme.color.lightorange3
                font.pixelSize: 13
                font.family: "Share Tech Mono"
                text: Chymaera.serverRunning
                      ? qsTr("API :%1").arg(Chymaera.serverPort)
                      : qsTr("API off")
            }
        }

        Text {
            Layout.fillWidth: true
            color: Theme.color.mediumorange1
            font.pixelSize: 12
            font.family: "Share Tech Mono"
            elide: Text.ElideMiddle
            text: (Chymaera.sessionActive
                   ? qsTr("Session: %1").arg(Chymaera.sessionName)
                   : qsTr("No active session"))
                  + "   •   " + Chymaera.datastorePath
        }

        // --- device + capture pull ------------------------------------------
        RowLayout {
            Layout.fillWidth: true
            spacing: 8

            Rectangle {
                width: 10; height: 10; radius: 5
                color: Chymaera.deviceConnected ? Theme.color.lightgreen : Theme.color.mediumorange1
            }
            Text {
                color: Theme.color.lightorange3
                font.pixelSize: 12
                font.family: "Share Tech Mono"
                text: Chymaera.deviceConnected
                      ? qsTr("Device: %1").arg(Chymaera.deviceName || qsTr("connected"))
                      : qsTr("No device")
            }
            Item { Layout.fillWidth: true }
            Button {
                text: qsTr("Pull NFC")
                enabled: Chymaera.deviceConnected
                onClicked: Chymaera.pullPath("/ext/nfc")
            }
            Button {
                text: qsTr("Pull Sub-GHz")
                enabled: Chymaera.deviceConnected
                onClicked: Chymaera.pullPath("/ext/subghz")
            }
        }

        // --- live log --------------------------------------------------------
        Rectangle {
            Layout.fillWidth: true
            Layout.fillHeight: true
            color: Theme.color.darkorange2
            border.color: Theme.color.mediumorange3
            border.width: 1
            radius: 4
            clip: true

            ListView {
                id: logView
                anchors.fill: parent
                anchors.margins: 6
                model: Chymaera.eventLog
                spacing: 2
                boundsBehavior: Flickable.StopAtBounds

                ScrollBar.vertical: ScrollBar { }

                delegate: RowLayout {
                    width: ListView.view.width
                    spacing: 8

                    Text {
                        text: (model.timestamp || "").slice(11, 23)
                        color: Theme.color.mediumorange1
                        font.pixelSize: 12
                        font.family: "Share Tech Mono"
                    }
                    Text {
                        text: (model.sourceName || "").toUpperCase()
                        color: Theme.color.lightorange3
                        font.pixelSize: 12
                        font.family: "Share Tech Mono"
                        Layout.minimumWidth: 62
                    }
                    Text {
                        Layout.fillWidth: true
                        text: model.message + (model.detail ? "  —  " + model.detail : "")
                        color: root.severityColor(model.severityName)
                        font.pixelSize: 12
                        font.family: "Share Tech Mono"
                        wrapMode: Text.Wrap
                    }
                }

                // Follow the tail as new entries arrive.
                onCountChanged: positionViewAtEnd()
            }

            Text {
                anchors.centerIn: parent
                visible: logView.count === 0
                text: qsTr("No events yet")
                color: Theme.color.mediumorange1
                font.family: "Share Tech Mono"
            }
        }

        // --- manual note input ----------------------------------------------
        RowLayout {
            Layout.fillWidth: true
            spacing: 8

            TextField {
                id: noteField
                Layout.fillWidth: true
                placeholderText: qsTr("Log a note…")
                color: Theme.color.lightorange2
                font.family: "Share Tech Mono"
                background: Rectangle {
                    color: Theme.color.darkorange2
                    border.color: Theme.color.mediumorange3
                    radius: 4
                }
                onAccepted: root.submitNote()
            }
            Button {
                text: qsTr("Add")
                onClicked: root.submitNote()
            }
        }

        // --- controls --------------------------------------------------------
        RowLayout {
            Layout.fillWidth: true
            spacing: 8

            Button {
                text: Chymaera.sessionActive ? qsTr("End session") : qsTr("Start session")
                onClicked: {
                    if(Chymaera.sessionActive) {
                        Chymaera.endSession();
                    } else {
                        Chymaera.startSession(Qt.formatDateTime(new Date(), "yyyy-MM-dd hh:mm"), "");
                    }
                }
            }
            Button {
                text: Chymaera.serverRunning ? qsTr("Stop API") : qsTr("Start API")
                onClicked: {
                    if(Chymaera.serverRunning) {
                        Chymaera.stopServer();
                    } else {
                        Chymaera.startServer(44700, "");
                    }
                }
            }
            Item { Layout.fillWidth: true }
            Button {
                text: qsTr("Clear")
                onClicked: Chymaera.clearLog()
            }
        }
    }

    function submitNote() {
        const text = noteField.text.trim();
        if(text.length > 0) {
            Chymaera.log(text);
            noteField.clear();
        }
    }
}
