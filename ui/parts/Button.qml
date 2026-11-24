import QtQuick
import ".."

// A word that acts; disabled ones say why on hover.
MouseArea {
    id: button
    property string word
    property string hint: ""
    property bool enabledWord: true
    property bool selected: false
    property color tone: Theme.foreground

    implicitWidth: label.implicitWidth + 14
    implicitHeight: 22
    hoverEnabled: true
    cursorShape: enabledWord ? Qt.PointingHandCursor : Qt.ArrowCursor

    Rectangle {
        anchors.fill: parent
        radius: Theme.radius
        color: button.selected ? Theme.hover : button.containsMouse && button.enabledWord ? Theme.hover : "transparent"
        border { width: 1; color: Theme.border }
    }
    Text {
        id: label
        anchors.centerIn: parent
        text: button.word
        color: button.enabledWord ? button.tone : Theme.quiet
        font { family: Theme.font; pixelSize: Theme.small }
    }
    Hint { shown: button.containsMouse && button.hint.length > 0; words: button.hint }
}
