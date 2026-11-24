pragma ComponentBehavior: Bound
import QtQuick
import ".."

// Every registered campaign under its division in arrival order, each with its state word; the add-to-view and remove gestures; scrolls for dozens; no frame around any group.
Rectangle {
    id: panel
    color: Theme.sunken
    clip: true

    Rectangle { anchors { right: parent.right; top: parent.top; bottom: parent.bottom } width: 1; color: Theme.border }

    Flickable {
        id: list
        anchors { fill: parent; topMargin: 8; bottomMargin: 8 }
        contentHeight: sections.implicitHeight
        boundsBehavior: Flickable.StopAtBounds
        clip: true

        Column {
            id: sections
            width: list.width
            Repeater {
                model: Words.divisions
                delegate: Column {
                    id: division
                    required property string modelData
                    readonly property var members: Backend.listed.filter(c => (c.division || "none") === modelData)
                    width: sections.width
                    visible: members.length > 0
                    Text {
                        visible: division.modelData !== "none"
                        leftPadding: 14; topPadding: 12; bottomPadding: 4
                        text: division.modelData
                        color: division.modelData === "Citium" ? Theme.citium : division.modelData === "OVRIN" ? Theme.ovrin : Theme.zavent
                        font { family: Theme.font; pixelSize: Theme.small; weight: Font.DemiBold }
                    }
                    Repeater {
                        model: division.members
                        delegate: CampaignItem { width: sections.width }
                    }
                }
            }
        }

        Rectangle {
            visible: list.contentHeight > list.height
            x: list.width - 6
            y: list.contentY + (list.height - height) * list.contentY / Math.max(1, list.contentHeight - list.height)
            width: 4; radius: 2
            color: Theme.border
            height: Math.max(24, list.height * list.height / Math.max(1, list.contentHeight))
        }
    }

    Text {
        anchors.centerIn: parent
        visible: Backend.listed.length === 0
        text: Backend.connected || Backend.lastHeard > 0 ? Words.noCampaign : Words.lost
        color: Theme.quiet
        font { family: Theme.font; pixelSize: Theme.normal }
    }
}
