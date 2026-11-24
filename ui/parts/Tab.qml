import QtQuick
import ".."

MouseArea {
    id: tab
    property string word
    property bool shown: false

    width: label.implicitWidth + 20
    height: parent ? parent.height : 40
    hoverEnabled: true
    cursorShape: Qt.PointingHandCursor

    Text {
        id: label
        anchors.centerIn: parent
        text: tab.word
        color: tab.shown || tab.containsMouse ? Theme.foreground : Theme.quiet
        font { family: Theme.font; pixelSize: Theme.normal; weight: tab.shown ? Font.DemiBold : Font.Normal }
    }
    Rectangle {
        anchors { left: label.left; right: label.right; bottom: parent.bottom; bottomMargin: 6 }
        height: 2
        radius: 1
        color: Theme.citium
        visible: tab.shown
    }
}
