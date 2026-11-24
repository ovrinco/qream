pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Shapes
import ".."

// One header across the window: the icon-only mark, the four tabs, the notice slot, the text size
// with its lock, the sound toggle, the age when stale, Settings.
Rectangle {
    id: header
    property string page: "Execution"
    signal chosen(string name)

    height: 40
    color: Theme.sunken

    Rectangle { anchors { left: parent.left; right: parent.right; bottom: parent.bottom } height: 1; color: Theme.border }

    MouseArea {
        id: mark
        width: 40; height: 40
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: Backend.leftFolded = !Backend.leftFolded

        // The Citium pyramid; on hover a chevron the way the left panel will move.
        Shape {
            anchors.centerIn: parent
            width: 22; height: 20
            visible: !mark.containsMouse
            preferredRendererType: Shape.CurveRenderer
            ShapePath {
                strokeWidth: 0
                fillColor: Theme.citium
                startX: 11; startY: 0
                PathLine { x: 22; y: 20 }
                PathLine { x: 0; y: 20 }
                PathLine { x: 11; y: 0 }
            }
        }
        Shape {
            anchors.centerIn: parent
            width: 10; height: 16
            visible: mark.containsMouse
            preferredRendererType: Shape.CurveRenderer
            ShapePath {
                strokeColor: Theme.citium
                strokeWidth: 2.5
                fillColor: "transparent"
                capStyle: ShapePath.RoundCap
                startX: Backend.leftFolded ? 2 : 8; startY: 1
                PathLine { x: Backend.leftFolded ? 8 : 2; y: 8 }
                PathLine { x: Backend.leftFolded ? 2 : 8; y: 15 }
            }
        }
    }

    Row {
        id: tabs
        anchors { left: mark.right; leftMargin: 8; top: parent.top; bottom: parent.bottom }
        spacing: 4
        Repeater {
            model: Words.tabs
            delegate: Tab {
                required property string modelData
                word: modelData
                shown: header.page === modelData
                onClicked: header.chosen(modelData)
            }
        }
    }

    // The notice slot: the daemon's one line for ten seconds; a click opens the latest lines.
    MouseArea {
        id: slot
        anchors { left: tabs.right; leftMargin: 24; right: controls.left; rightMargin: 16; top: parent.top; bottom: parent.bottom }
        hoverEnabled: true
        cursorShape: Backend.notices.length > 0 ? Qt.PointingHandCursor : Qt.ArrowCursor
        onClicked: list.visible = !list.visible && Backend.notices.length > 0
        Text {
            id: line
            anchors { left: parent.left; right: parent.right; verticalCenter: parent.verticalCenter }
            text: Backend.notice
            elide: Text.ElideRight
            maximumLineCount: 1
            color: Theme.foreground
            font { family: Theme.font; pixelSize: Theme.normal }
        }
        Hint { shown: slot.containsMouse && line.truncated; words: Backend.notice }
        Rectangle {
            id: list
            visible: false
            z: 100
            y: parent.height + 4
            width: Math.min(560, slot.width)
            height: notices.implicitHeight + 16
            radius: Theme.radius
            color: Theme.surface
            border { width: 1; color: Theme.border }
            Column {
                id: notices
                anchors { left: parent.left; right: parent.right; top: parent.top; margins: 8 }
                spacing: 4
                Text {
                    text: Words.latestNotices
                    color: Theme.quiet
                    font { family: Theme.font; pixelSize: Theme.small }
                }
                Repeater {
                    model: Backend.notices
                    delegate: Text {
                        required property var modelData
                        width: notices.width
                        text: Qt.formatTime(modelData.at, "HH:mm") + "  " + modelData.line
                        wrapMode: Text.WordWrap
                        color: Theme.foreground
                        font { family: Theme.font; pixelSize: Theme.small }
                    }
                }
            }
        }
    }

    Row {
        id: controls
        anchors { right: parent.right; rightMargin: 8; top: parent.top; bottom: parent.bottom }
        spacing: 12
        // Text size for the panes, with the lock beside it: locked reaches every pane, open the focused one.
        Row {
            anchors.verticalCenter: parent.verticalCenter
            spacing: 2
            Button { word: Words.refresh; hint: Words.refreshHint; onClicked: Backend.refreshAll() }
            Button { word: "−"; hint: Words.smaller; onClicked: Backend.stepSize(-1) }
            Button { word: "A"; hint: Words.defaultSize; selected: Backend.textSize === 0 && Object.keys(Backend.paneSizes).length === 0; onClicked: Backend.stepSize(0) }
            Button { word: "+"; hint: Words.larger; onClicked: Backend.stepSize(1) }
            MouseArea {
                id: lock
                width: 26; height: 22
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: Backend.sizeLock = !Backend.sizeLock
                Rectangle { anchors.fill: parent; radius: Theme.radius; color: lock.containsMouse ? Theme.hover : "transparent" }
                // A padlock drawn in two shapes: the shackle closes when the lock is on.
                Rectangle {
                    x: 8; y: Backend.sizeLock ? 4 : 1
                    width: 10; height: 9; radius: 5
                    color: "transparent"
                    border { width: 1.5; color: Theme.foreground }
                }
                Rectangle { x: 6; y: 10; width: 14; height: 9; radius: 2; color: Theme.foreground }
                Hint { shown: lock.containsMouse; words: Backend.sizeLock ? Words.lockOn : Words.lockOff }
            }
        }
        // The sound toggle: a speaker, struck through when muted (RW-09-R04).
        MouseArea {
            id: speaker
            anchors.verticalCenter: parent.verticalCenter
            width: 26; height: 22
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: Backend.muted = !Backend.muted
            Rectangle { anchors.fill: parent; radius: Theme.radius; color: speaker.containsMouse ? Theme.hover : "transparent" }
            Shape {
                anchors.fill: parent
                preferredRendererType: Shape.CurveRenderer
                ShapePath {
                    strokeWidth: 0
                    fillColor: Backend.muted ? Theme.quiet : Theme.foreground
                    startX: 6; startY: 9
                    PathLine { x: 9; y: 9 }
                    PathLine { x: 13; y: 5 }
                    PathLine { x: 13; y: 17 }
                    PathLine { x: 9; y: 13 }
                    PathLine { x: 6; y: 13 }
                    PathLine { x: 6; y: 9 }
                }
                ShapePath {
                    strokeColor: Backend.muted ? Theme.red : Theme.foreground
                    strokeWidth: 1.5
                    fillColor: "transparent"
                    capStyle: ShapePath.RoundCap
                    startX: Backend.muted ? 15 : 16; startY: Backend.muted ? 8 : 8
                    PathLine { x: Backend.muted ? 21 : 18; y: Backend.muted ? 14 : 11 }
                    PathMove { x: Backend.muted ? 21 : 16; y: Backend.muted ? 8 : 14 }
                    PathLine { x: Backend.muted ? 15 : 18; y: Backend.muted ? 14 : 11 }
                }
            }
            Hint { shown: speaker.containsMouse; words: Backend.muted ? Words.unmuteHint : Words.muteHint }
        }
        Text {
            anchors.verticalCenter: parent.verticalCenter
            visible: Backend.stale || !Backend.connected
            text: Backend.connected ? Words.stale : Words.lost
            color: Theme.quiet
            font { family: Theme.font; pixelSize: Theme.small }
        }
        Tab {
            word: Words.orchestrator
            shown: Backend.seatOpen
            onClicked: Backend.seatOpen = !Backend.seatOpen
        }
        Tab {
            word: Words.settings
            shown: header.page === "Settings"
            onClicked: header.chosen("Settings")
        }
    }
}
