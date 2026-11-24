import QtQuick
import ".."

Row {
    id: row
    property string label
    default property alias controls: holder.data
    spacing: 16
    Text {
        width: 180
        anchors.verticalCenter: parent.verticalCenter
        text: row.label
        color: Theme.foreground
        font { family: Theme.font; pixelSize: Theme.normal }
    }
    Row { id: holder; spacing: 6 }
}
