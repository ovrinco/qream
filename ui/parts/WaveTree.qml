pragma ComponentBehavior: Bound
import QtQuick
import ".."

// The wave tree as served: the current wave open, held waves with their reason, what remains;
// waves before the three newest fold into "Earlier waves (n)". Each task: title, word, trace line.
Flickable {
    id: tree
    property var cstate: null
    property string campaign
    property string selected
    property var opened: ({})
    property bool earlierOpen: false
    signal picked(string task)

    readonly property int current: cstate ? cstate.current_wave : 0
    // A Prepared campaign has not been triggered: every wave waits for the trigger. With no wave
    // served yet, its waves up to the declared last wave are listed held.
    readonly property bool prepared: !!cstate && cstate.word === "prepared"
    readonly property var waves: {
        const served = cstate && cstate.waves ? cstate.waves : [];
        if (served.length || !prepared || !(cstate.last_wave > 0))
            return served;
        return Array.from({
            length: cstate.last_wave
        }, (_, i) => ({
                    n: i + 1,
                    state: "held",
                    remaining: 0
                }));
    }
    function reason(w) {
        return prepared ? "held, waiting for the trigger" : (w.reason || "");
    }
    // Waves older than the three up to the current one fold away.
    readonly property int foldAt: Math.max(0, waves.findIndex(w => w.n === current) - 2)

    function isOpen(w) {
        return opened[w.n] !== undefined ? opened[w.n] : (w.state === "open" || w.n === current);
    }
    function toggle(w) {
        const o = Object.assign({}, opened);
        o[w.n] = !isOpen(w);
        opened = o;
    }

    clip: true
    contentHeight: column.implicitHeight
    boundsBehavior: Flickable.StopAtBounds

    Column {
        id: column
        width: tree.width
        spacing: 2

        MouseArea {
            visible: tree.foldAt > 0
            width: parent.width
            height: visible ? 24 : 0
            cursorShape: Qt.PointingHandCursor
            onClicked: tree.earlierOpen = !tree.earlierOpen
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: "Earlier waves (" + tree.foldAt + ")"
                color: Theme.quiet
                font { family: Theme.font; pixelSize: Theme.small }
            }
        }

        Repeater {
            model: tree.waves
            delegate: Column {
                id: wave
                required property var modelData
                required property int index
                width: column.width
                visible: index >= tree.foldAt || tree.earlierOpen
                spacing: 2

                MouseArea {
                    width: parent.width
                    height: head.implicitHeight + 8
                    cursorShape: Qt.PointingHandCursor
                    onClicked: tree.toggle(wave.modelData)
                    Text {
                        id: head
                        anchors { left: parent.left; right: parent.right; verticalCenter: parent.verticalCenter }
                        wrapMode: Text.WordWrap
                        text: "Wave " + wave.modelData.n + (tree.reason(wave.modelData) ? " · " + tree.reason(wave.modelData) : "") + (wave.modelData.remaining > 0 ? " · " + wave.modelData.remaining + (wave.modelData.remaining === 1 ? " task left" : " tasks left") : "")
                        color: wave.modelData.state === "open" && !tree.prepared ? Theme.foreground : Theme.quiet
                        font { family: Theme.font; pixelSize: Theme.small; weight: Font.DemiBold }
                    }
                }

                Repeater {
                    model: tree.isOpen(wave.modelData) && tree.cstate ? tree.cstate.tasks.filter(t => t.wave === wave.modelData.n) : []
                    delegate: MouseArea {
                        id: task
                        required property var modelData
                        // A card has one fixed size: two lines of title and one state word. Nothing on
                        // it grows or shrinks when the state changes, so the list never moves.
                        width: column.width
                        height: 2 * Math.ceil(Theme.small * 1.4) + 12
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: tree.picked(modelData.task)

                        Rectangle {
                            anchors.fill: parent
                            radius: Theme.radius
                            color: tree.selected === task.modelData.task || task.containsMouse ? Theme.hover : "transparent"
                            border.width: tree.selected === task.modelData.task ? 1 : 0
                            border.color: Theme.quiet
                        }
                        Text {
                            id: title
                            anchors { left: parent.left; right: word.left; top: parent.top; leftMargin: 8; rightMargin: 8; topMargin: 6 }
                            text: Words.title(task.modelData)
                            wrapMode: Text.WordWrap
                            maximumLineCount: 2
                            elide: Text.ElideRight
                            color: Theme.foreground
                            font { family: Theme.font; pixelSize: Theme.small }
                        }
                        Text {
                            id: word
                            anchors { right: parent.right; top: parent.top; rightMargin: 8; topMargin: 6 }
                            width: 72
                            horizontalAlignment: Text.AlignRight
                            text: Words.task(task.modelData)
                            color: Words.taskColour(task.modelData.word)
                            font { family: Theme.font; pixelSize: Theme.small }
                        }
                    }
                }
            }
        }
    }
}
