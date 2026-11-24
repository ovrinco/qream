pragma ComponentBehavior: Bound
import QtQuick
import ".."

// The replay's timeline: one tick per event in its accent, the From and To bounds as handles, the
// playhead; dragging a handle moves that bound, dragging anywhere else seeks both ways.
Item {
    id: line

    property var times: []
    property var kinds: []
    property double start: 0
    property double end: 1
    property double from: 0
    property double to: 1
    property double playhead: 0
    signal seek(double at)
    signal bounds(double from, double to)

    readonly property real inset: 8
    function xOf(t) {
        return inset + (width - 2 * inset) * (end > start ? (t - start) / (end - start) : 0);
    }
    function tOf(x) {
        return start + (end - start) * Math.max(0, Math.min(1, (x - inset) / (width - 2 * inset)));
    }
    onTimesChanged: ticks.requestPaint()
    onWidthChanged: ticks.requestPaint()

    Rectangle {
        x: line.xOf(line.from)
        width: Math.max(1, line.xOf(line.to) - x)
        anchors { top: parent.top; bottom: parent.bottom }
        color: Theme.hover
    }
    Canvas {
        id: ticks
        anchors.fill: parent
        onPaint: {
            const ctx = getContext("2d");
            ctx.reset();
            for (let i = 0; i < line.times.length; i++) {
                const k = line.kinds[i];
                ctx.fillStyle = k === "write" ? "#d0a36b" : k === "read" ? "#92bdb0" : Qt.rgba(Theme.muted.r, Theme.muted.g, Theme.muted.b, 0.5);
                const h = k === "write" || k === "read" ? height * 0.7 : height * 0.35;
                ctx.fillRect(Math.round(line.xOf(line.times[i])), (height - h) / 2, 1, h);
            }
        }
    }
    Repeater {
        model: [line.from, line.to]
        delegate: Rectangle {
            required property double modelData
            x: line.xOf(modelData) - 1
            width: 3
            anchors { top: parent.top; bottom: parent.bottom }
            color: Theme.foreground
            opacity: 0.7
        }
    }
    Rectangle {
        x: line.xOf(line.playhead) - 1
        width: 2
        anchors { top: parent.top; bottom: parent.bottom; topMargin: -4; bottomMargin: -4 }
        color: Theme.citium
    }
    MouseArea {
        anchors.fill: parent
        property string grip: ""
        onPressed: e => {
            const near = x => Math.abs(e.x - x) < 6;
            grip = near(line.xOf(line.from)) ? "from" : near(line.xOf(line.to)) ? "to" : "seek";
            move(e.x);
        }
        onPositionChanged: e => move(e.x)
        function move(x) {
            const t = line.tOf(x);
            if (grip === "from")
                line.bounds(Math.min(t, line.to), line.to);
            else if (grip === "to")
                line.bounds(line.from, Math.max(t, line.from));
            else
                line.seek(t);
        }
    }
}
