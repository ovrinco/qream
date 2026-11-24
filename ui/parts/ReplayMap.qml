pragma ComponentBehavior: Bound
import QtQuick
import ".."

// The disk replay in MindWalk's family: a tilted map of districts (one per folder), one block per
// path whose height grows with its visits up to the playhead, coloured by its last act; reads and
// writes carry MindWalk's two accents, every other act the window's neutrals; a fresh event pulses.
Item {
    id: map
    clip: true

    // Nodes: [{path, district, x, z, times:[ms], kinds:[word]}]; districts: [{name, x, z, width, depth}].
    property var nodes: []
    property var districts: []
    property double playhead: 0
    property double from: 0
    property double pulse: 60000
    property real yaw: -28
    property real pitch: -48
    property real zoom: 1
    property int painted: 0

    readonly property color readAccent: "#92bdb0"
    readonly property color writeAccent: "#d0a36b"
    readonly property color unvisited: "#353e40"
    readonly property color ground: "#243033"
    readonly property color edge: "#465050"

    // How many of a node's events fall inside [from, playhead]; times are sorted.
    function upto(times, t) {
        let lo = 0, hi = times.length;
        while (lo < hi) {
            const mid = (lo + hi) >> 1;
            if (times[mid] <= t)
                lo = mid + 1;
            else
                hi = mid;
        }
        return lo;
    }
    function tone(kind) {
        return kind === "write" ? writeAccent : kind === "read" ? readAccent : Theme.muted;
    }
    function repaint() {
        scene.requestPaint();
    }
    onPlayheadChanged: repaint()
    onFromChanged: repaint()
    onNodesChanged: repaint()
    onYawChanged: repaint()
    onZoomChanged: repaint()

    Canvas {
        id: scene
        anchors.fill: parent
        renderStrategy: Canvas.Cooperative

        property real cx: 0
        property real cy: 0
        function raw(x, y, z) {
            const a = map.yaw * Math.PI / 180, b = -map.pitch * Math.PI / 180;
            const rx = x * Math.cos(a) - z * Math.sin(a), rz = x * Math.sin(a) + z * Math.cos(a);
            return {
                x: rx,
                y: rz * Math.sin(b) - y * Math.cos(b),
                depth: rz
            };
        }
        function project(x, y, z, scale) {
            const r = raw(x, y, z);
            return {
                x: width / 2 + (r.x - cx) * scale,
                y: height / 2 + (r.y - cy) * scale,
                depth: r.depth
            };
        }
        // Fits the districts' projected corners inside the canvas, with room for the tallest block.
        function fit() {
            let x0 = 1e9, x1 = -1e9, y0 = 1e9, y1 = -1e9;
            for (const d of map.districts)
                for (const sx of [-1, 1])
                    for (const sz of [-1, 1])
                        for (const h of [0, 5]) {
                            const r = raw(d.x + sx * d.width / 2, h, d.z + sz * d.depth / 2);
                            x0 = Math.min(x0, r.x);
                            x1 = Math.max(x1, r.x);
                            y0 = Math.min(y0, r.y);
                            y1 = Math.max(y1, r.y);
                        }
            if (x0 > x1)
                return 1;
            cx = (x0 + x1) / 2;
            cy = (y0 + y1) / 2;
            return Math.min((width - 48) / Math.max(1, x1 - x0), (height - 48) / Math.max(1, y1 - y0)) * map.zoom;
        }
        function polygon(ctx, p, fill) {
            ctx.beginPath();
            ctx.moveTo(p[0].x, p[0].y);
            for (let i = 1; i < p.length; i++)
                ctx.lineTo(p[i].x, p[i].y);
            ctx.closePath();
            ctx.fillStyle = fill;
            ctx.fill();
            ctx.stroke();
        }

        onPaint: {
            const ctx = getContext("2d");
            ctx.reset();
            ctx.strokeStyle = map.edge;
            ctx.lineWidth = 0.6;
            const scale = scene.fit();
            for (const d of map.districts)
                polygon(ctx, [scene.project(d.x - d.width / 2, 0, d.z - d.depth / 2, scale), scene.project(d.x + d.width / 2, 0, d.z - d.depth / 2, scale), scene.project(d.x + d.width / 2, 0, d.z + d.depth / 2, scale), scene.project(d.x - d.width / 2, 0, d.z + d.depth / 2, scale)], map.ground);
            const blocks = [];
            for (const n of map.nodes) {
                const end = map.upto(n.times, map.playhead);
                const seen = end - map.upto(n.times, map.from - 1);
                const top = scene.project(n.x, 0, n.z, scale);
                blocks.push({
                    n: n,
                    seen: seen,
                    last: seen > 0 ? n.kinds[end - 1] : "",
                    fresh: seen > 0 && map.playhead - n.times[end - 1] < map.pulse,
                    depth: top.depth
                });
            }
            blocks.sort((p, q) => p.depth - q.depth);
            const s = 0.42;
            for (const b of blocks) {
                const n = b.n, h = b.seen > 0 ? 0.25 + Math.min(4.5, b.seen * 0.35) : 0.06;
                const colour = b.seen > 0 ? map.tone(b.last) : map.unvisited;
                const c = [[-s, -s], [s, -s], [s, s], [-s, s]];
                const bottom = c.map(k => scene.project(n.x + k[0], 0, n.z + k[1], scale));
                const top = c.map(k => scene.project(n.x + k[0], h, n.z + k[1], scale));
                if (b.seen > 0) {
                    polygon(ctx, [bottom[1], bottom[2], top[2], top[1]], Qt.darker(colour, 1.45));
                    polygon(ctx, [bottom[2], bottom[3], top[3], top[2]], Qt.darker(colour, 1.8));
                }
                polygon(ctx, top, colour);
                if (b.fresh) {
                    const p = scene.project(n.x, h, n.z, scale);
                    const age = (map.playhead - n.times[map.upto(n.times, map.playhead) - 1]) / map.pulse;
                    ctx.beginPath();
                    ctx.arc(p.x, p.y, 3 + age * 7, 0, Math.PI * 2);
                    ctx.strokeStyle = Qt.rgba(colour.r, colour.g, colour.b, 1 - age);
                    ctx.lineWidth = 2;
                    ctx.stroke();
                    ctx.strokeStyle = map.edge;
                    ctx.lineWidth = 0.6;
                }
            }
            ctx.fillStyle = Theme.quiet;
            ctx.font = Theme.small + "px '" + Theme.font + "'";
            // Name the larger districts only, so the map stays readable.
            for (const d of map.districts) {
                if (d.width < 4 && map.districts.length > 12)
                    continue;
                const p = scene.project(d.x - d.width / 2, 0, d.z - d.depth / 2, scale);
                ctx.fillText(d.name, p.x + 4, p.y - 4);
            }
        }
        onPainted: map.painted += 1
    }

    // Drag to turn the map, the wheel to zoom.
    MouseArea {
        anchors.fill: parent
        property real lastX: 0
        onPressed: e => lastX = e.x
        onPositionChanged: e => {
            map.yaw += (e.x - lastX) * 0.3;
            lastX = e.x;
        }
        onWheel: e => map.zoom = Math.max(0.4, Math.min(4, map.zoom * (e.angleDelta.y > 0 ? 1.1 : 0.9)))
    }

    Row {
        anchors { left: parent.left; bottom: parent.bottom; margins: 12 }
        spacing: 16
        Repeater {
            model: [["Read", map.readAccent], ["Write", map.writeAccent], ["Other", Theme.muted]]
            delegate: Row {
                id: key
                required property var modelData
                spacing: 6
                Rectangle { width: 9; height: 9; radius: 2; color: key.modelData[1]; anchors.verticalCenter: parent.verticalCenter }
                Text { text: key.modelData[0]; color: Theme.quiet; font { family: Theme.font; pixelSize: Theme.small } }
            }
        }
    }
}
