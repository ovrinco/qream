import QtQuick
import ".."

MouseArea {
    id: item
    required property var modelData
    readonly property string name: modelData.name
    readonly property var cstate: Backend.states[name]
    readonly property bool shown: Backend.inView(name)

    height: 32
    hoverEnabled: true
    acceptedButtons: Qt.LeftButton
    onClicked: mouse => {
        if (mouse.modifiers & Qt.ControlModifier)
            Backend.toggleView(name);
    }

    Rectangle {
        anchors { fill: parent; leftMargin: 6; rightMargin: 6 }
        radius: Theme.radius
        color: item.containsMouse ? Theme.hover : "transparent"
    }
    Rectangle {
        visible: item.shown
        x: 10; anchors.verticalCenter: parent.verticalCenter
        width: 6; height: 6
        color: Theme.foreground
    }
    Text {
        id: label
        anchors { left: parent.left; leftMargin: 22; right: tail.left; rightMargin: 6; verticalCenter: parent.verticalCenter }
        text: item.modelData.title || item.name
        elide: Text.ElideNone
        wrapMode: Text.NoWrap
        clip: true
        color: Theme.foreground
        font { family: Theme.font; pixelSize: Theme.normal }
    }
    Row {
        id: tail
        anchors { right: parent.right; rightMargin: 12; verticalCenter: parent.verticalCenter }
        spacing: 6
        Text {
            anchors.verticalCenter: parent.verticalCenter
            visible: text.length > 0 && !(item.containsMouse && item.cstate.word === "stalled")
            text: Words.campaign(item.cstate)
            color: item.cstate ? Words.campaignColour(item.cstate.word) : Theme.muted
            font { family: Theme.font; pixelSize: Theme.small }
        }
        Button {
            visible: !!item.cstate && item.cstate.word === "prepared" && item.containsMouse
            word: "Trigger"
            onClicked: Backend.trigger(item.name)
        }
        Button {
            visible: item.containsMouse
            word: item.shown ? "Remove from view" : "Add to view"
            onClicked: Backend.toggleView(item.name)
        }
        Rectangle {
            anchors.verticalCenter: parent.verticalCenter
            visible: !item.containsMouse && !!item.cstate && item.cstate.word === "running"
            width: 6; height: 6; radius: 3
            color: Theme.blue
        }
    }
}
