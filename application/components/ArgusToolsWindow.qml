import QtQuick 2.15
import QtQuick.Window 2.15
import QtQuick.Layouts 1.15
import QtQuick.Controls 2.15

import Theme 1.0
import QFlipper 1.0

/* Argus tools: apps from firmware packages, the app check-up, Limbo rebuilds, SD card
 * snapshots and the capture library. Everything here talks to Chymaera.argus
 * (backend/argus/arguscontroller.h). Text from packages, the card and Limbo guests is hostile:
 * every such string is shown as plain text. */
Window {
    id: win

    width: 1180
    height: 720
    minimumWidth: 820
    minimumHeight: 560
    title: qsTr("Argus tools")
    color: Theme.color.darkorange1

    readonly property var argus: Chymaera.argus
    readonly property string mono: "Share Tech Mono"

    visible: argus.smokeTest

    // shared state between pages
    property var selectedApps: ({})
    property var checkupPackages: []
    property var planItems: []
    property string rebuildCategory: "Misc"
    property string rebuildTarget: ""

    function shortHash(h) { return h ? h.substring(0, 12) : ""; }
    function sizeText(n) {
        if(n === undefined) return "";
        if(n >= 1048576) return (n / 1048576).toFixed(1) + " MB";
        if(n >= 1024) return (n / 1024).toFixed(1) + " KB";
        return n + " B";
    }
    function statusColor(s) {
        switch(s) {
        case "compatible": return Theme.color.lightgreen;
        case "incompatible": return Theme.color.lightred2;
        default: return Theme.color.lightorange1;
        }
    }
    function note(text) {
        messages.insert(0, { line: Qt.formatTime(new Date(), "hh:mm:ss") + "  " + text });
        if(messages.count > 200) messages.remove(200, messages.count - 200);
    }
    function openFile(filters, callback) {
        const handler = function() {
            SystemFileDialog.accepted.disconnect(handler);
            callback(SystemFileDialog.fileUrl);
        };
        SystemFileDialog.accepted.connect(handler);
        SystemFileDialog.beginOpenFile(SystemFileDialog.LastLocation, filters);
    }
    function openDir(callback) {
        const handler = function() {
            SystemFileDialog.accepted.disconnect(handler);
            callback(SystemFileDialog.fileUrl);
        };
        SystemFileDialog.accepted.connect(handler);
        SystemFileDialog.beginOpenDir(SystemFileDialog.LastLocation);
    }
    function askInstall(apps, force) {
        if(apps.length === 0) { note(qsTr("Nothing selected")); return; }
        argus.prepareInstall(apps, force);
    }

    ListModel { id: messages }

    Connections {
        target: win.argus
        function onOperationFinished(op, ok, message) {
            if(message.length > 0) win.note((ok ? "" : qsTr("FAILED ")) + op + ": " + message);
            if(op === "installPlan" && ok) {
                win.planItems = win.argus.installPlan.map(function(i) { return Object.assign({}, i); });
                if(win.planItems.length > 0) planPopup.open();
            }
            if(op === "restorePlan" && ok) restorePopup.open();
        }
    }

    component Btn: SmallButton {
        implicitHeight: 30
    }

    component Label2: Text {
        color: Theme.color.lightorange2
        font.family: win.mono
        font.pixelSize: 13
        textFormat: Text.PlainText
        elide: Text.ElideRight
    }

    component Field: TextField {
        color: Theme.color.lightorange2
        font.family: win.mono
        selectByMouse: true
        background: Rectangle { color: Theme.color.darkorange2; border.color: Theme.color.mediumorange3; radius: 4 }
    }

    component Panel: Rectangle {
        color: Theme.color.darkorange2
        border.color: Theme.color.mediumorange3
        border.width: 1
        radius: 4
        clip: true
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 12
        spacing: 8

        // ---- header ----------------------------------------------------------------------------
        RowLayout {
            Layout.fillWidth: true
            spacing: 12
            Text {
                text: qsTr("Argus tools")
                color: Theme.color.lightorange2
                font.family: win.mono
                font.pixelSize: 20
            }
            Rectangle { width: 10; height: 10; radius: 5; color: win.argus.cardReady ? Theme.color.lightgreen : Theme.color.mediumorange1 }
            Label2 {
                text: win.argus.cardReady ? qsTr("Card: %1").arg(win.argus.cardName) : qsTr("No Flipper connected")
                Layout.maximumWidth: 260
            }
            Label2 {
                text: qsTr("Firmware API %1%2").arg(win.argus.firmwareApi).arg(win.argus.firmwareApiFromDevice ? "" : qsTr(" (default)"))
                color: Theme.color.lightorange3
            }
            Item { Layout.fillWidth: true }
            BusyIndicator { running: win.argus.busy; implicitWidth: 24; implicitHeight: 24 }
            Label2 {
                text: win.argus.available ? "" : qsTr("argus-tools binary missing")
                color: Theme.color.lightred2
                visible: !win.argus.available
            }
        }

        Label2 {
            Layout.fillWidth: true
            text: win.argus.status
            color: Theme.color.mediumorange1
        }
        ProgressBar {
            Layout.fillWidth: true
            visible: win.argus.busy
            indeterminate: win.argus.progress < 0
            value: win.argus.progress < 0 ? 0 : win.argus.progress
        }

        RowLayout {
            id: tabs
            Layout.fillWidth: true
            spacing: 6
            property int currentIndex: 0
            Repeater {
                model: [ qsTr("Apps"), qsTr("Check-up"), qsTr("Rebuild"), qsTr("Snapshots"), qsTr("Captures") ]
                delegate: Btn {
                    text: modelData
                    highlighted: tabs.currentIndex === index
                    Layout.preferredWidth: 130
                    onClicked: tabs.currentIndex = index
                }
            }
            Item { Layout.fillWidth: true }
        }

        StackLayout {
            id: pages
            Layout.fillWidth: true
            Layout.fillHeight: true
            currentIndex: tabs.currentIndex

            // ---- Apps: firmware package extractor ----------------------------------------------
            ColumnLayout {
                spacing: 8
                RowLayout {
                    Layout.fillWidth: true
                    Btn {
                        text: qsTr("Open package…")
                        enabled: !win.argus.busy
                        onClicked: win.openFile([ "Firmware packages (*.tgz *.zip *.tar *.gz *.ths *.fap)", "All files (*)" ],
                                                function(url) { packagePath.text = url.toString(); win.argus.scanPackage(url.toString()); })
                    }
                    Field { id: packagePath; Layout.fillWidth: true; placeholderText: qsTr("…or a path to a .tgz / .zip / .tar / resources.ths / .fap") }
                    Btn { text: qsTr("Scan"); enabled: !win.argus.busy && packagePath.text.length > 0; onClicked: win.argus.scanPackage(packagePath.text) }
                }
                Label2 {
                    Layout.fillWidth: true
                    text: win.argus.packageReport.package
                          ? qsTr("%1 (%2) — %3 apps, %4 skipped%5").arg(win.argus.packageReport.package).arg(win.argus.packageReport.format)
                                .arg((win.argus.packageReport.apps || []).length).arg((win.argus.packageReport.skipped || []).length)
                                .arg(win.argus.packageReport.incomplete ? qsTr(" — INCOMPLETE: a size limit was hit") : "")
                          : qsTr("Argus opens the package as data only; nothing in it is run. Apps are checked against the device's API with the firmware loader's rule.")
                    wrapMode: Text.Wrap
                }
                Panel {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    ListView {
                        id: appList
                        objectName: "argusAppList"
                        anchors.fill: parent
                        anchors.margins: 6
                        clip: true
                        model: win.argus.packageReport.apps || []
                        ScrollBar.vertical: ScrollBar {}
                        delegate: RowLayout {
                            width: ListView.view.width
                            spacing: 8
                            readonly property var app: modelData
                            CheckBox {
                                checked: !!win.selectedApps[app.sha256 + app.package_path]
                                onToggled: { const s = win.selectedApps; s[app.sha256 + app.package_path] = checked; win.selectedApps = s; }
                            }
                            Label2 { text: app.manifest ? app.manifest.name : app.file_name; Layout.preferredWidth: 200 }
                            Label2 { text: app.category; Layout.preferredWidth: 90; color: Theme.color.lightorange3 }
                            Label2 { text: win.sizeText(app.size); Layout.preferredWidth: 70 }
                            Label2 { text: app.manifest ? "API " + app.manifest.api_major + "." + app.manifest.api_minor : "-"; Layout.preferredWidth: 80 }
                            Label2 {
                                text: app.compat ? app.compat.status : "?"
                                color: win.statusColor(app.compat ? app.compat.status : "")
                                Layout.preferredWidth: 100
                                ToolTip.visible: tipArea.containsMouse
                                ToolTip.text: app.compat ? app.compat.reason : (app.error ? app.error.message : "")
                                MouseArea { id: tipArea; anchors.fill: parent; hoverEnabled: true }
                            }
                            Label2 { text: win.shortHash(app.sha256); Layout.preferredWidth: 110; color: Theme.color.mediumorange1 }
                            Label2 { text: app.package_path; Layout.fillWidth: true; elide: Text.ElideMiddle; color: Theme.color.mediumorange1 }
                        }
                    }
                }
                RowLayout {
                    Layout.fillWidth: true
                    Btn {
                        text: qsTr("Select compatible")
                        onClicked: {
                            const s = {};
                            (win.argus.packageReport.apps || []).forEach(function(a) { if(a.compat && a.compat.status === "compatible") s[a.sha256 + a.package_path] = true; });
                            win.selectedApps = s;
                        }
                    }
                    Btn { text: qsTr("Clear"); onClicked: win.selectedApps = ({}) }
                    CheckBox { id: forceBox }
                    Label2 { text: qsTr("Force: also install apps that will not load") }
                    Item { Layout.fillWidth: true }
                    Btn {
                        text: qsTr("Install selected…")
                        enabled: win.argus.cardReady && !win.argus.busy
                        onClicked: win.askInstall((win.argus.packageReport.apps || []).filter(function(a) { return !!win.selectedApps[a.sha256 + a.package_path]; }), forceBox.checked)
                    }
                }
            }

            // ---- Check-up -------------------------------------------------------------------------
            ColumnLayout {
                spacing: 8
                RowLayout {
                    Layout.fillWidth: true
                    Btn {
                        text: qsTr("Run check-up")
                        enabled: win.argus.cardReady && !win.argus.busy
                        onClicked: win.argus.runCheckup(win.checkupPackages)
                    }
                    Btn {
                        text: qsTr("Add package…")
                        onClicked: win.openFile([ "Firmware packages (*.tgz *.zip *.tar *.gz *.ths)", "All files (*)" ],
                                                function(url) { win.checkupPackages = win.checkupPackages.concat([url.toString()]); })
                    }
                    Btn { text: qsTr("Clear packages"); onClicked: win.checkupPackages = [] }
                    Label2 { Layout.fillWidth: true; text: win.checkupPackages.length ? qsTr("%1 package(s) given").arg(win.checkupPackages.length) : "" }
                }
                Label2 {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    text: win.argus.checkupReport.apps
                          ? qsTr("%1 apps on the card; %2 will not load on API %3, %4 unknown").arg(win.argus.checkupReport.apps.length)
                                .arg(win.argus.checkupReport.incompatible).arg(win.argus.checkupReport.firmware_api).arg(win.argus.checkupReport.unknown)
                          : qsTr("Reads every app in /ext/apps over USB and checks its manifest and imports against the firmware.")
                }
                Panel {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    ListView {
                        anchors.fill: parent
                        anchors.margins: 6
                        clip: true
                        ScrollBar.vertical: ScrollBar {}
                        model: (win.argus.checkupReport.apps || []).slice().sort(function(a, b) {
                            return (a.compat.status === "compatible") - (b.compat.status === "compatible");
                        })
                        delegate: RowLayout {
                            width: ListView.view.width
                            spacing: 8
                            readonly property var app: modelData
                            Label2 { text: app.path; Layout.preferredWidth: 300; elide: Text.ElideMiddle }
                            Label2 { text: app.compat.app_api ? "API " + app.compat.app_api : "-"; Layout.preferredWidth: 80 }
                            Label2 { text: app.compat.status; color: win.statusColor(app.compat.status); Layout.preferredWidth: 100 }
                            Label2 { text: app.compat.reason; Layout.fillWidth: true; color: Theme.color.mediumorange1 }
                            Btn {
                                visible: !!app.replacement
                                text: qsTr("Install compatible copy")
                                enabled: !win.argus.busy
                                onClicked: {
                                    const r = Object.assign({}, app.replacement);
                                    r.target = app.path;
                                    win.argus.prepareInstall([r], false);
                                }
                            }
                            Btn {
                                visible: app.compat.status !== "compatible"
                                text: qsTr("Rebuild…")
                                onClicked: {
                                    win.rebuildTarget = app.path;
                                    win.rebuildCategory = app.category || "Misc";
                                    tabs.currentIndex = 2;
                                }
                            }
                        }
                    }
                }
            }

            // ---- Rebuild in Limbo -----------------------------------------------------------------
            ColumnLayout {
                spacing: 8
                Label2 {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    text: qsTr("Builds the app with ufbt for API %1 inside disposable Limbo guests; app source is never built on this PC. A git URL is cloned in a guest with network, then built in one without. See argus-tools/limbo/README.md for the one-time root setup.").arg(win.argus.firmwareApi)
                }
                RowLayout {
                    Layout.fillWidth: true
                    Field { id: rebuildSource; Layout.fillWidth: true; placeholderText: qsTr("https://github.com/owner/repo  or a local app folder (with application.fam)") }
                    Btn { text: qsTr("Folder…"); onClicked: win.openDir(function(url) { rebuildSource.text = url.toString(); }) }
                }
                RowLayout {
                    Layout.fillWidth: true
                    Field { id: rebuildRef; Layout.preferredWidth: 200; placeholderText: qsTr("branch or tag (optional)") }
                    Field { id: rebuildSubdir; Layout.fillWidth: true; placeholderText: qsTr("app folder inside the repository (optional)") }
                    Field { id: rebuildCat; Layout.preferredWidth: 140; text: win.rebuildCategory; placeholderText: qsTr("category") }
                    Btn {
                        text: qsTr("Build in Limbo")
                        enabled: !win.argus.busy && rebuildSource.text.length > 0
                        onClicked: win.argus.rebuild(rebuildSource.text, rebuildRef.text, rebuildSubdir.text)
                    }
                }
                Label2 { visible: win.rebuildTarget.length > 0; text: qsTr("Will replace %1").arg(win.rebuildTarget); color: Theme.color.lightorange3 }
                Panel {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 110
                    ListView {
                        anchors.fill: parent
                        anchors.margins: 6
                        model: win.argus.rebuildReport.faps || []
                        delegate: RowLayout {
                            width: ListView.view.width
                            spacing: 8
                            readonly property var fap: modelData
                            Label2 { text: fap.manifest ? fap.manifest.name : fap.file_name; Layout.preferredWidth: 200 }
                            Label2 { text: fap.file_name; Layout.preferredWidth: 180 }
                            Label2 { text: fap.compat.status; color: win.statusColor(fap.compat.status); Layout.preferredWidth: 100 }
                            Label2 { text: fap.compat.reason; Layout.fillWidth: true; color: Theme.color.mediumorange1 }
                            Btn {
                                text: qsTr("Install…")
                                enabled: win.argus.cardReady && !win.argus.busy
                                onClicked: {
                                    const f = Object.assign({}, fap);
                                    f.category = rebuildCat.text || "Misc";
                                    if(win.rebuildTarget.length > 0) f.target = win.rebuildTarget;
                                    win.argus.prepareInstall([f], false);
                                }
                            }
                        }
                    }
                }
                Label2 { text: qsTr("Build log (from the guest)") }
                Panel {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    ScrollView {
                        anchors.fill: parent
                        anchors.margins: 4
                        TextArea {
                            readOnly: true
                            textFormat: TextEdit.PlainText
                            text: win.argus.rebuildReport.log_tail || ""
                            color: Theme.color.lightorange2
                            font.family: win.mono
                            font.pixelSize: 12
                            wrapMode: TextEdit.WrapAnywhere
                        }
                    }
                }
            }

            // ---- Snapshots ------------------------------------------------------------------------
            ColumnLayout {
                spacing: 8
                RowLayout {
                    Layout.fillWidth: true
                    Field { id: snapLabel; Layout.preferredWidth: 260; placeholderText: qsTr("label (optional)") }
                    Btn {
                        text: qsTr("Take snapshot")
                        enabled: win.argus.cardReady && !win.argus.busy
                        onClicked: win.argus.takeSnapshot(snapLabel.text)
                    }
                    Btn { text: qsTr("Refresh"); onClicked: win.argus.refreshSnapshots() }
                    Item { Layout.fillWidth: true }
                    Label2 { text: qsTr("Kept in ~/.local/share/argus/snapshots (private; each file stored once)"); color: Theme.color.mediumorange1 }
                }
                Panel {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 200
                    ListView {
                        id: snapList
                        anchors.fill: parent
                        anchors.margins: 6
                        clip: true
                        ScrollBar.vertical: ScrollBar {}
                        model: win.argus.snapshots.slice().reverse()
                        delegate: RowLayout {
                            width: ListView.view.width
                            spacing: 8
                            readonly property var snap: modelData
                            Label2 { text: snap.id; Layout.preferredWidth: 190 }
                            Label2 { text: snap.created + " UTC"; Layout.preferredWidth: 170; color: Theme.color.lightorange3 }
                            Label2 { text: snap.label; Layout.preferredWidth: 140 }
                            Label2 { text: qsTr("%1 files, %2").arg(snap.file_count).arg(win.sizeText(snap.total_bytes)); Layout.fillWidth: true }
                            Btn { text: qsTr("Diff with card"); enabled: win.argus.cardReady && !win.argus.busy; onClicked: win.argus.diffSnapshot(snap.id, "") }
                            Btn {
                                text: qsTr("Diff with newest")
                                enabled: !win.argus.busy && win.argus.snapshots.length > 1 && index > 0
                                onClicked: win.argus.diffSnapshot(snap.id, win.argus.snapshots[win.argus.snapshots.length - 1].id)
                            }
                            Btn { text: qsTr("Restore…"); enabled: win.argus.cardReady && !win.argus.busy; onClicked: win.argus.planRestore(snap.id, restorePath.text) }
                            Btn { text: qsTr("Delete"); enabled: !win.argus.busy; onClicked: win.argus.deleteSnapshot(snap.id) }
                        }
                    }
                }
                RowLayout {
                    Layout.fillWidth: true
                    Label2 { text: qsTr("Restore only:") }
                    Field { id: restorePath; Layout.fillWidth: true; placeholderText: qsTr("a file or folder on the card, e.g. /ext/subghz/garage.sub (empty: the whole card)") }
                }
                Label2 {
                    Layout.fillWidth: true
                    text: win.argus.snapshotDiff.from
                          ? qsTr("Diff %1 → %2: %3 added, %4 removed, %5 changed, %6 unknown, %7 unchanged").arg(win.argus.snapshotDiff.from).arg(win.argus.snapshotDiff.to)
                                .arg(win.argus.snapshotDiff.added.length).arg(win.argus.snapshotDiff.removed.length).arg(win.argus.snapshotDiff.changed.length)
                                .arg(win.argus.snapshotDiff.unknown.length).arg(win.argus.snapshotDiff.unchanged)
                          : ""
                }
                Panel {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    ListView {
                        anchors.fill: parent
                        anchors.margins: 6
                        clip: true
                        ScrollBar.vertical: ScrollBar {}
                        model: {
                            const d = win.argus.snapshotDiff;
                            if(!d.from) return [];
                            const rows = [];
                            (d.added || []).forEach(function(c) { rows.push({ kind: "added", c: c }); });
                            (d.removed || []).forEach(function(c) { rows.push({ kind: "removed", c: c }); });
                            (d.changed || []).forEach(function(c) { rows.push({ kind: "changed", c: c }); });
                            (d.unknown || []).forEach(function(c) { rows.push({ kind: "same size?", c: c }); });
                            return rows;
                        }
                        delegate: RowLayout {
                            width: ListView.view.width
                            Label2 {
                                text: modelData.kind
                                Layout.preferredWidth: 100
                                color: modelData.kind === "added" ? Theme.color.lightgreen : modelData.kind === "removed" ? Theme.color.lightred2 : Theme.color.lightorange1
                            }
                            Label2 { text: modelData.c.path; Layout.fillWidth: true; elide: Text.ElideMiddle }
                            Label2 { text: win.sizeText(modelData.c.old_size) + " → " + win.sizeText(modelData.c.new_size); Layout.preferredWidth: 200 }
                        }
                    }
                }
            }

            // ---- Captures -------------------------------------------------------------------------
            ColumnLayout {
                spacing: 8
                RowLayout {
                    Layout.fillWidth: true
                    Btn { text: qsTr("Read from Flipper"); enabled: win.argus.cardReady && !win.argus.busy; onClicked: win.argus.readCapturesFromCard() }
                    Btn { text: qsTr("Rebuild index"); enabled: !win.argus.busy; onClicked: win.argus.buildCaptureLibrary() }
                    Btn { text: qsTr("Duplicates"); onClicked: win.argus.findDuplicates() }
                    Field {
                        id: captureQuery
                        Layout.fillWidth: true
                        placeholderText: qsTr("search: words, kind:nfc, proto:princeton, freq:433.92, uid:04a1, tag:garage")
                        onAccepted: win.argus.searchCaptures(text)
                    }
                    Btn { text: qsTr("Search"); onClicked: win.argus.searchCaptures(captureQuery.text) }
                }
                Label2 {
                    Layout.fillWidth: true
                    text: qsTr("Sub-GHz, infrared, NFC, RFID and iButton files from the card, the console's pulls and every snapshot. The index keeps header facts only; captures never leave this PC.")
                    color: Theme.color.mediumorange1
                    wrapMode: Text.Wrap
                }
                RowLayout {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    spacing: 8
                    Panel {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        ListView {
                            id: captureList
                            objectName: "argusCaptureList"
                            anchors.fill: parent
                            anchors.margins: 6
                            clip: true
                            ScrollBar.vertical: ScrollBar {}
                            model: win.argus.captures
                            delegate: ItemDelegate {
                                width: ListView.view.width
                                height: 26
                                onClicked: win.argus.explainCapture(modelData.id)
                                contentItem: RowLayout {
                                    spacing: 8
                                    Label2 { text: modelData.kind; Layout.preferredWidth: 90; color: Theme.color.lightorange3 }
                                    Label2 { text: modelData.name; Layout.preferredWidth: 150 }
                                    Label2 { text: modelData.summary; Layout.fillWidth: true; color: Theme.color.mediumorange1 }
                                    Label2 { text: (modelData.tags || []).join(", "); Layout.preferredWidth: 100; color: Theme.color.lightgreen }
                                }
                            }
                        }
                    }
                    Panel {
                        Layout.preferredWidth: 380
                        Layout.fillHeight: true
                        ColumnLayout {
                            anchors.fill: parent
                            anchors.margins: 8
                            spacing: 6
                            readonly property var cap: win.argus.captureDetail.capture
                            Label2 { text: parent.cap ? parent.cap.name + "  (" + parent.cap.id + ")" : qsTr("Pick a capture"); font.pixelSize: 15 }
                            Text {
                                Layout.fillWidth: true
                                textFormat: Text.PlainText
                                wrapMode: Text.Wrap
                                color: Theme.color.lightorange2
                                font.family: win.mono
                                font.pixelSize: 12
                                text: win.argus.captureDetail.explanation || ""
                            }
                            RowLayout {
                                Layout.fillWidth: true
                                visible: !!parent.cap
                                Field { id: tagField; Layout.fillWidth: true; placeholderText: qsTr("tag") }
                                Btn { text: qsTr("Add"); onClicked: { win.argus.tagCapture(parent.parent.cap.id, [tagField.text], []); tagField.clear(); } }
                                Btn { text: qsTr("Remove"); onClicked: { win.argus.tagCapture(parent.parent.cap.id, [], [tagField.text]); tagField.clear(); } }
                            }
                            Label2 { text: qsTr("Duplicates"); visible: win.argus.duplicates.length > 0 }
                            ListView {
                                Layout.fillWidth: true
                                Layout.fillHeight: true
                                clip: true
                                model: win.argus.duplicates
                                delegate: Label2 {
                                    width: ListView.view.width
                                    wrapMode: Text.Wrap
                                    elide: Text.ElideNone
                                    text: (modelData.kind === "same_file" ? qsTr("same file: ") : qsTr("same signal: ")) + modelData.paths.join(", ")
                                    color: Theme.color.lightorange3
                                }
                            }
                        }
                    }
                }
            }
        }

        // ---- messages ------------------------------------------------------------------------------
        Panel {
            Layout.fillWidth: true
            Layout.preferredHeight: 90
            ListView {
                anchors.fill: parent
                anchors.margins: 4
                clip: true
                model: messages
                delegate: Label2 { width: ListView.view.width; text: model.line; font.pixelSize: 12; color: Theme.color.lightorange3 }
            }
        }
    }

    // ---- install confirmation: lists every file that exists on the card ---------------------------
    Popup {
        id: planPopup
        anchors.centerIn: parent
        width: Math.min(win.width - 60, 860)
        height: Math.min(win.height - 60, 520)
        modal: true
        background: Rectangle { color: Theme.color.darkorange1; border.color: Theme.color.mediumorange3; radius: 6 }
        ColumnLayout {
            anchors.fill: parent
            spacing: 8
            Label2 { text: qsTr("Install to the Flipper"); font.pixelSize: 16 }
            Label2 {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                text: qsTr("Files already on the card are kept unless you tick Overwrite. Refused apps are not written.")
            }
            Panel {
                Layout.fillWidth: true
                Layout.fillHeight: true
                ListView {
                    anchors.fill: parent
                    anchors.margins: 6
                    clip: true
                    model: win.planItems
                    delegate: RowLayout {
                        width: ListView.view.width
                        spacing: 8
                        Label2 { text: modelData.remote; Layout.preferredWidth: 330; elide: Text.ElideMiddle }
                        Label2 {
                            Layout.preferredWidth: 90
                            text: modelData.refused ? qsTr("refused") : modelData.exists ? qsTr("exists") : qsTr("new")
                            color: modelData.refused ? Theme.color.lightred2 : modelData.exists ? Theme.color.lightorange1 : Theme.color.lightgreen
                        }
                        Label2 { visible: modelData.exists && !modelData.refused; text: qsTr("Overwrite") }
                        CheckBox {
                            visible: modelData.exists && !modelData.refused
                            checked: modelData.overwrite
                            onToggled: win.planItems[index].overwrite = checked
                        }
                        Label2 { text: modelData.refused ? modelData.reason : ""; Layout.fillWidth: true; color: Theme.color.mediumorange1 }
                    }
                }
            }
            RowLayout {
                Layout.fillWidth: true
                Item { Layout.fillWidth: true }
                Btn { text: qsTr("Cancel"); onClicked: planPopup.close() }
                Btn {
                    text: qsTr("Install")
                    onClicked: { planPopup.close(); win.argus.runInstall(win.planItems); }
                }
            }
        }
    }

    // ---- restore confirmation: lists what will be overwritten -------------------------------------
    Popup {
        id: restorePopup
        anchors.centerIn: parent
        width: Math.min(win.width - 60, 860)
        height: Math.min(win.height - 60, 520)
        modal: true
        background: Rectangle { color: Theme.color.darkorange1; border.color: Theme.color.mediumorange3; radius: 6 }
        readonly property var plan: win.argus.restorePlan
        ColumnLayout {
            anchors.fill: parent
            spacing: 8
            Label2 { text: qsTr("Restore from %1").arg(restorePopup.plan.snapshot || ""); font.pixelSize: 16 }
            Label2 {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                text: qsTr("%1 file(s) will be OVERWRITTEN on the card and %2 created; %3 already match and are left alone. Nothing is deleted.%4")
                        .arg(restorePopup.plan.overwrites || 0).arg(restorePopup.plan.creates || 0).arg(restorePopup.plan.same || 0)
                        .arg((restorePopup.plan.missing_objects || []).length ? qsTr(" %1 file(s) cannot be restored (missing from the store).").arg(restorePopup.plan.missing_objects.length) : "")
            }
            Panel {
                Layout.fillWidth: true
                Layout.fillHeight: true
                ListView {
                    anchors.fill: parent
                    anchors.margins: 6
                    clip: true
                    model: (restorePopup.plan.items || []).filter(function(i) { return i.action !== "same"; })
                    delegate: RowLayout {
                        width: ListView.view.width
                        Label2 {
                            Layout.preferredWidth: 150
                            text: modelData.action === "create" ? qsTr("create") : qsTr("OVERWRITE")
                            color: modelData.action === "create" ? Theme.color.lightgreen : Theme.color.lightred2
                        }
                        Label2 { text: modelData.remote; Layout.fillWidth: true; elide: Text.ElideMiddle }
                        Label2 { text: win.sizeText(modelData.size); Layout.preferredWidth: 90 }
                    }
                }
            }
            RowLayout {
                Layout.fillWidth: true
                Item { Layout.fillWidth: true }
                Btn { text: qsTr("Cancel"); onClicked: restorePopup.close() }
                Btn {
                    text: qsTr("Restore (overwrite %1)").arg(restorePopup.plan.overwrites || 0)
                    onClicked: { restorePopup.close(); win.argus.runRestore(); }
                }
            }
        }
    }

    // ---- offscreen UI check (ARGUS_QML_SMOKE) ----------------------------------------------------
    function shot(name) {
        const path = win.argus.smokeShotPath(name);
        if(path.length > 0) win.contentItem.grabToImage(function(r) { r.saveToFile(path); });
    }

    Timer {
        id: smokeTimer
        interval: 300
        repeat: true
        property int step: 0
        property string failures: ""
        running: win.argus.smokeTest
        onTriggered: {
            switch(step++) {
            case 0: tabs.currentIndex = 0; if(win.argus.smokeArg.length > 1) win.argus.scanPackage(win.argus.smokeArg); break;
            case 1: case 2: case 3: case 4: case 5: break; // let the scan finish
            case 6:
                if(win.argus.smokeArg.length > 1 && appList.count === 0) failures += "no apps listed; ";
                win.shot("apps.png"); break;
            case 7: tabs.currentIndex = 1; break;
            case 8: win.shot("checkup.png"); tabs.currentIndex = 2; break;
            case 9: win.shot("rebuild.png"); tabs.currentIndex = 3; win.argus.refreshSnapshots(); break;
            case 10: win.shot("snapshots.png"); tabs.currentIndex = 4; win.argus.searchCaptures(""); break;
            case 11: if(captureList.count === 0) failures += "no captures listed; "; break;
            case 12: win.argus.explainCapture(win.argus.captures.length ? win.argus.captures[0].id : "00000000"); break;
            case 13: win.shot("captures.png"); planPopup.open(); break;
            case 14: planPopup.close(); restorePopup.open(); break;
            case 15: restorePopup.close(); break;
            default:
                running = false;
                win.argus.smokeFinished(failures.length === 0, "apps listed: " + appList.count + ", captures listed: " + captureList.count + "; " + failures);
            }
        }
    }
}
