import QtQuick
import "../parts"
import ".."

// The monitoring surface: only the panels the operator added.
Item {
    PanelGrid { anchors { fill: parent; margins: 8 } }
    Text {
        anchors.centerIn: parent
        visible: Backend.view.length === 0
        text: Words.addOne
        color: Theme.quiet
        font { family: Theme.font; pixelSize: Theme.normal }
    }
}
