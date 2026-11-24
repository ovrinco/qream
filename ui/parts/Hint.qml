import QtQuick
import ".."

// A hover sentence under its owner.
Rectangle {
    property bool shown: false
    property string words
    visible: shown
    z: 100
    y: parent ? parent.height + 4 : 0
    x: parent ? Math.min(0, parent.width - width) : 0
    width: Math.min(320, hint.implicitWidth + 12)
    height: hint.implicitHeight + 8
    radius: Theme.radius
    color: Theme.surface
    border { width: 1; color: Theme.border }
    Text {
        id: hint
        anchors { fill: parent; margins: 6 }
        text: parent.words
        wrapMode: Text.WordWrap
        color: Theme.foreground
        font { family: Theme.font; pixelSize: Theme.small }
    }
}
