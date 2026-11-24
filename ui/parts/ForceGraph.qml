import QtQuick
import ".."

// A campaign's graph after Obsidian's graph view: round nodes on a dark field, thin grey links, a force
// layout settled before first draw, wheel zoom about the pointer, drag to pan, a node dragged pulls its
// neighbours, hover lights a node and its neighbours and dims the rest, labels grow in with zoom.
// Two node kinds: a task, filled with its state colour; a row it serves, a ring in its status tone.
// Links: task to row (serves), and a task to the task of the wave before it that serves the same row (order).
Item {
    id: graph

    property string campaign: ""
    property var tasks: []
    property var rowStatus: ({})
    property string selected: ""
    property string hovered: ""
    readonly property var hoveredNode: hovered === "" ? null : nodes[index[hovered]] || null

    // Measured for the report: the settle time and frame times while a node is dragged.
    property real layoutMs: 0
    property var frameGaps: []
    property var paintTimes: []

    signal picked(var node)

    property var nodes: []
    property var links: []
    property var index: ({})
    property var neighbours: ({})
    property string shape: ""
    property real zoom: 1
    property real ox: 0
    property real oy: 0
    property real alpha: 0
    property int pinned: -1
    property double lastTick: 0

    readonly property real taskRadius: 9
    readonly property real rowRadius: 6

    function key(t) {
        return t.task + ":" + t.wave + ":" + (t.rows || []).map(r => r.id).join(",");
    }
    function hash(text) {
        let h = 2166136261;
        for (let i = 0; i < text.length; i++)
            h = Math.imul(h ^ text.charCodeAt(i), 16777619);
        return ((h >>> 0) % 10000) / 10000;
    }

    // Rebuild the nodes only when the set of tasks or rows changes; a word change only repaints.
    function update() {
        const next = graph.campaign + "|" + tasks.map(key).join("|");
        if (next !== shape) {
            shape = next;
            build();
        } else {
            canvas.requestPaint();
        }
    }

    function build() {
        const old = {};
        for (let i = 0; i < nodes.length; i++)
            old[nodes[i].id] = nodes[i];
        const ns = [];
        const ix = {};
        const ls = [];
        const waves = tasks.map(t => t.wave || 0);
        const lo = Math.min.apply(null, waves.concat([0]));
        const hi = Math.max.apply(null, waves.concat([0]));
        const mid = (lo + hi) / 2;
        const add = n => {
            const was = old[n.id];
            n.x = was ? was.x : n.x;
            n.y = was ? was.y : n.y;
            n.vx = 0;
            n.vy = 0;
            ix[n.id] = ns.length;
            ns.push(n);
        };
        for (let i = 0; i < tasks.length; i++) {
            const t = tasks[i];
            add({
                id: "task:" + t.task,
                kind: "task",
                task: t.task,
                wave: t.wave || 0,
                bias: ((t.wave || 0) - mid) * 150,
                label: t.title || t.task,
                x: ((t.wave || 0) - mid) * 150 + (hash(t.task) - 0.5) * 80,
                y: (hash(t.task + "y") - 0.5) * 300
            });
        }
        const lastOfRow = {};
        const byWave = tasks.slice().sort((a, b) => (a.wave || 0) - (b.wave || 0));
        for (let i = 0; i < byWave.length; i++) {
            const t = byWave[i];
            const ti = ix["task:" + t.task];
            const rows = t.rows || [];
            for (let j = 0; j < rows.length; j++) {
                const r = rows[j];
                const rid = "row:" + r.id;
                if (ix[rid] === undefined)
                    add({
                        id: rid,
                        kind: "row",
                        row: r.id,
                        label: r.words || r.id,
                        x: ns[ti].x + (hash(r.id) - 0.5) * 60,
                        y: ns[ti].y + (hash(r.id + "y") - 0.5) * 60
                    });
                ls.push({
                    a: ti,
                    b: ix[rid],
                    kind: "serves"
                });
                const before = lastOfRow[r.id];
                if (before !== undefined && before.wave < (t.wave || 0) && !ls.some(l => l.kind === "order" && l.a === before.node && l.b === ti))
                    ls.push({
                        a: before.node,
                        b: ti,
                        kind: "order"
                    });
                lastOfRow[r.id] = {
                    node: ti,
                    wave: t.wave || 0
                };
            }
        }
        const nb = {};
        for (let i = 0; i < ns.length; i++)
            nb[i] = {};
        for (let i = 0; i < ls.length; i++) {
            nb[ls[i].a][ls[i].b] = true;
            nb[ls[i].b][ls[i].a] = true;
        }
        nodes = ns;
        links = ls;
        index = ix;
        neighbours = nb;
        // Settled before the first draw (design sheet 3.4), then fitted.
        const t0 = Date.now();
        let a = 1;
        while (a > 0.004) {
            step(a);
            a *= 0.982;
        }
        layoutMs = Date.now() - t0;
        alpha = 0;
        fit();
    }

    function step(a) {
        const ns = nodes;
        const n = ns.length;
        for (let i = 0; i < n; i++) {
            const p = ns[i];
            for (let j = i + 1; j < n; j++) {
                const q = ns[j];
                let dx = p.x - q.x;
                let dy = p.y - q.y;
                let d2 = dx * dx + dy * dy;
                if (d2 < 1) {
                    dx = (i - j) * 0.1 + 0.1;
                    dy = 0.1;
                    d2 = 1;
                }
                if (d2 > 160000)
                    continue;
                const f = 1100 * a / d2;
                p.vx += dx * f;
                p.vy += dy * f;
                q.vx -= dx * f;
                q.vy -= dy * f;
            }
        }
        const ls = links;
        for (let k = 0; k < ls.length; k++) {
            const p = ns[ls[k].a];
            const q = ns[ls[k].b];
            const rest = ls[k].kind === "serves" ? 46 : 110;
            const dx = q.x - p.x;
            const dy = q.y - p.y;
            const d = Math.sqrt(dx * dx + dy * dy) || 1;
            const f = (d - rest) / d * 0.09 * a * (ls[k].kind === "serves" ? 1 : 0.4);
            p.vx += dx * f;
            p.vy += dy * f;
            q.vx -= dx * f;
            q.vy -= dy * f;
        }
        for (let i = 0; i < n; i++) {
            const p = ns[i];
            if (p.kind === "task")
                p.vx += (p.bias - p.x) * 0.012 * a;
            p.vx -= p.x * 0.004 * a;
            p.vy -= p.y * 0.005 * a;
            if (i === pinned) {
                p.vx = 0;
                p.vy = 0;
                continue;
            }
            p.vx *= 0.6;
            p.vy *= 0.6;
            p.x += p.vx;
            p.y += p.vy;
        }
    }

    // The whole graph in the view, labels included, with a 32 px margin, centred. Labels keep their
    // size on screen, so their room is set aside in pixels before the scale is chosen.
    function fit() {
        if (nodes.length === 0 || width <= 0 || height <= 0) {
            canvas.requestPaint();
            return;
        }
        let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
        for (let i = 0; i < nodes.length; i++) {
            const p = nodes[i];
            x0 = Math.min(x0, p.x);
            y0 = Math.min(y0, p.y);
            x1 = Math.max(x1, p.x);
            y1 = Math.max(y1, p.y);
        }
        const margin = 32;
        const r = taskRadius * 1.6 + 4;
        const side = margin + Math.max(r, labelRoom / 2 + 2);
        const top = margin + r;
        const bottom = margin + r + Theme.small + 6;
        const s = Math.min((width - 2 * side) / Math.max(x1 - x0, 1), (height - top - bottom) / Math.max(y1 - y0, 1));
        zoom = Math.max(0.05, Math.min(s, 2.2));
        ox = side + (width - 2 * side) / 2 - (x0 + x1) / 2 * zoom;
        oy = top + (height - top - bottom) / 2 - (y0 + y1) / 2 * zoom;
        canvas.requestPaint();
    }
    // The widest a task's label is drawn before it is cut.
    readonly property real labelRoom: 150

    function zoomAt(px, py, factor) {
        const s = Math.max(0.05, Math.min(zoom * factor, 6));
        ox = px - (px - ox) * s / zoom;
        oy = py - (py - oy) * s / zoom;
        zoom = s;
        canvas.requestPaint();
    }

    function nodeAt(px, py) {
        let best = -1;
        let bestD = Infinity;
        for (let i = 0; i < nodes.length; i++) {
            const p = nodes[i];
            const dx = p.x * zoom + ox - px;
            const dy = p.y * zoom + oy - py;
            const d = dx * dx + dy * dy;
            const r = Math.max(radius(p) * Math.max(zoom, 0.6), 7) + 3;
            if (d <= r * r && d < bestD) {
                best = i;
                bestD = d;
            }
        }
        return best;
    }
    function radius(p) {
        return p.kind === "task" ? taskRadius : rowRadius;
    }
    function task(id) {
        for (let i = 0; i < tasks.length; i++)
            if (tasks[i].task === id)
                return tasks[i];
        return null;
    }

    // While a node is dragged or the layout is reheated, a step and a paint per frame.
    function heat(a) {
        alpha = Math.max(alpha, a);
        if (!ticker.running) {
            lastTick = 0;
            ticker.start();
        }
    }
    Timer {
        id: ticker
        interval: 16
        repeat: true
        onTriggered: {
            const now = Date.now();
            if (graph.pinned >= 0 && graph.lastTick > 0) {
                const g = graph.frameGaps;
                g.push(now - graph.lastTick);
                if (g.length > 600)
                    g.shift();
            }
            graph.lastTick = now;
            graph.step(graph.alpha);
            graph.alpha *= graph.pinned >= 0 ? 1 : 0.95;
            canvas.requestPaint();
            if (graph.alpha < 0.004 && graph.pinned < 0)
                stop();
        }
    }

    onTasksChanged: update()
    onRowStatusChanged: canvas.requestPaint()
    onSelectedChanged: canvas.requestPaint()
    onHoveredChanged: canvas.requestPaint()
    onWidthChanged: refit.restart()
    onHeightChanged: refit.restart()
    Timer {
        id: refit
        interval: 30
        onTriggered: graph.fit()
    }

    Canvas {
        id: canvas
        anchors.fill: parent
        renderStrategy: Canvas.Cooperative
        property var cut: ({})

        function elide(ctx, text, room) {
            const k = text + "|" + room;
            if (cut[k] !== undefined)
                return cut[k];
            let t = text;
            if (ctx.measureText(t).width > room) {
                while (t.length > 1 && ctx.measureText(t + "…").width > room)
                    t = t.slice(0, -1);
                t = t.replace(/\s+$/, "") + "…";
            }
            cut[k] = t;
            return t;
        }

        onPaint: {
            const t0 = Date.now();
            const ctx = getContext("2d");
            ctx.reset();
            ctx.fillStyle = Theme.background;
            ctx.fillRect(0, 0, width, height);
            const ns = graph.nodes;
            const s = graph.zoom;
            const focus = graph.hovered !== "" ? graph.index[graph.hovered] : graph.selected !== "" ? graph.index[graph.selected] : undefined;
            const lit = focus === undefined ? null : graph.neighbours[focus];
            const on = i => lit === null || i === focus || lit[i] === true;
            const sx = p => p.x * s + graph.ox;
            const sy = p => p.y * s + graph.oy;

            // Links: thin and grey; the focused node's links brighter.
            ctx.lineWidth = 1;
            for (let k = 0; k < graph.links.length; k++) {
                const l = graph.links[k];
                const near = lit !== null && (l.a === focus || l.b === focus);
                ctx.strokeStyle = Qt.rgba(Theme.foreground.r, Theme.foreground.g, Theme.foreground.b, near ? 0.55 : lit !== null ? 0.05 : (l.kind === "order" ? 0.12 : 0.2));
                ctx.beginPath();
                ctx.moveTo(sx(ns[l.a]), sy(ns[l.a]));
                ctx.lineTo(sx(ns[l.b]), sy(ns[l.b]));
                ctx.stroke();
            }

            const rs = Math.max(0.6, Math.min(s, 1.6));
            for (let i = 0; i < ns.length; i++) {
                const p = ns[i];
                const r = graph.radius(p) * rs;
                ctx.globalAlpha = on(i) ? 1 : 0.18;
                ctx.beginPath();
                ctx.arc(sx(p), sy(p), r, 0, Math.PI * 2);
                if (p.kind === "task") {
                    const t = graph.task(p.task);
                    const c = Words.taskColour(t ? t.word : "");
                    if (t && t.word === "waiting_on_job") {
                        ctx.lineWidth = 2;
                        ctx.strokeStyle = c;
                        ctx.stroke();
                    } else {
                        ctx.fillStyle = c;
                        ctx.fill();
                    }
                } else {
                    ctx.fillStyle = Theme.background;
                    ctx.fill();
                    ctx.lineWidth = 2;
                    ctx.strokeStyle = Words.rowTone(graph.rowStatus[p.row] || "");
                    ctx.stroke();
                }
                if (p.id === graph.selected) {
                    ctx.lineWidth = 2;
                    ctx.strokeStyle = Theme.citium;
                    ctx.beginPath();
                    ctx.arc(sx(p), sy(p), r + 4, 0, Math.PI * 2);
                    ctx.stroke();
                }
            }

            // Labels: task titles at any fitted scale (a label that would cover another waits), rows once zoomed;
            // the focused node and its neighbours always; cut to fit, whole on hover.
            ctx.font = Theme.small + "px '" + Theme.font + "'";
            ctx.textAlign = "center";
            ctx.textBaseline = "top";
            // A label that would cover one already placed waits for more zoom; the focus is placed first.
            const placed = [];
            const order = [];
            for (let i = 0; i < ns.length; i++)
                order.push(i);
            order.sort((a, b) => (b === focus) - (a === focus) || (ns[a].kind === "task" ? 0 : 1) - (ns[b].kind === "task" ? 0 : 1));
            for (let o = 0; o < order.length; o++) {
                const i = order[o];
                const p = ns[i];
                const near = lit !== null && on(i);
                const shown = near ? 1 : lit !== null ? 0 : p.kind === "task" ? Math.min(1, (s - 0.08) / 0.04) : Math.min(1, (s - 0.9) / 0.3);
                if (shown <= 0)
                    continue;
                ctx.globalAlpha = shown;
                ctx.fillStyle = p.kind === "task" ? Theme.foreground : Theme.quiet;
                const whole = graph.hovered === p.id;
                const text = whole ? p.label : elide(ctx, p.label, p.kind === "task" ? graph.labelRoom : 120);
                const w = ctx.measureText(text).width;
                const box = {
                    x0: sx(p) - w / 2 - 2,
                    x1: sx(p) + w / 2 + 2,
                    y0: sy(p) + graph.radius(p) * rs + 3,
                    y1: sy(p) + graph.radius(p) * rs + 5 + Theme.small
                };
                if (i !== focus && placed.some(b => b.x0 < box.x1 && box.x0 < b.x1 && b.y0 < box.y1 && box.y0 < b.y1))
                    continue;
                placed.push(box);
                ctx.fillText(text, sx(p), box.y0 + 1);
            }
            ctx.globalAlpha = 1;
            const pt = graph.paintTimes;
            pt.push(Date.now() - t0);
            if (pt.length > 600)
                pt.shift();
        }
    }

    MouseArea {
        id: mouse
        anchors.fill: parent
        hoverEnabled: true
        acceptedButtons: Qt.LeftButton
        property real px: 0
        property real py: 0
        property bool moved: false
        property bool panning: false

        onPressed: m => {
            px = m.x;
            py = m.y;
            moved = false;
            const i = graph.nodeAt(m.x, m.y);
            graph.pinned = i;
            panning = i < 0;
            if (i >= 0) {
                graph.frameGaps = [];
                graph.heat(0.3);
            }
        }
        onPositionChanged: m => {
            if (!pressed) {
                const i = graph.nodeAt(m.x, m.y);
                graph.hovered = i >= 0 ? graph.nodes[i].id : "";
                cursorShape = i >= 0 ? Qt.PointingHandCursor : Qt.ArrowCursor;
                return;
            }
            if (Math.abs(m.x - px) + Math.abs(m.y - py) > 3)
                moved = true;
            if (panning) {
                graph.ox += m.x - px;
                graph.oy += m.y - py;
                px = m.x;
                py = m.y;
                canvas.requestPaint();
            } else if (graph.pinned >= 0) {
                const p = graph.nodes[graph.pinned];
                p.x = (m.x - graph.ox) / graph.zoom;
                p.y = (m.y - graph.oy) / graph.zoom;
                graph.alpha = Math.max(graph.alpha, 0.3);
            }
        }
        onReleased: m => {
            const i = graph.pinned;
            graph.pinned = -1;
            if (!moved)
                graph.picked(i >= 0 ? graph.nodes[i] : null);
        }
        onExited: graph.hovered = ""
        onWheel: w => graph.zoomAt(w.x, w.y, Math.pow(1.0015, w.angleDelta.y))
    }

    // Automation for checks: drag a node in a circle for two seconds as a pointer would, then release.
    property int sweep: 0
    function sweepDrag(id) {
        const i = index[id] !== undefined ? index[id] : 0;
        frameGaps = [];
        paintTimes = [];
        pinned = i;
        sweep = 0;
        heat(0.3);
        sweeper.start();
    }
    Timer {
        id: sweeper
        interval: 16
        repeat: true
        property real cx: 0
        property real cy: 0
        onTriggered: {
            const p = graph.nodes[graph.pinned];
            if (!p) {
                stop();
                return;
            }
            if (graph.sweep === 0) {
                cx = p.x;
                cy = p.y;
            }
            graph.sweep += 1;
            p.x = cx + Math.sin(graph.sweep / 12) * 60;
            p.y = cy + (1 - Math.cos(graph.sweep / 12)) * 40;
            graph.alpha = Math.max(graph.alpha, 0.3);
            if (graph.sweep >= 125) {
                graph.pinned = -1;
                stop();
            }
        }
    }
    function stats() {
        const avg = a => a.length ? a.reduce((x, y) => x + y, 0) / a.length : 0;
        const worst = a => a.length ? Math.max.apply(null, a) : 0;
        const gap = avg(frameGaps);
        return JSON.stringify({
            nodes: nodes.length,
            links: links.length,
            layout_ms: layoutMs,
            frames: frameGaps.length,
            frame_gap_avg_ms: Math.round(gap * 10) / 10,
            frame_gap_worst_ms: worst(frameGaps),
            fps: gap > 0 ? Math.round(10000 / gap) / 10 : 0,
            paint_avg_ms: Math.round(avg(paintTimes) * 10) / 10,
            paint_worst_ms: worst(paintTimes),
            scale: Math.round(zoom * 100) / 100
        });
    }
}
