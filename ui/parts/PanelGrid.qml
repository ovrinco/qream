pragma ComponentBehavior: Bound
import QtQuick
import ".."

// The campaign panels the operator added, laid out for one to four: one fills the area, two sit
// side by side, three are two above and one wide below, four are two by two.
Item {
    id: grid
    readonly property int gap: 8

    function rect(i, n) {
        const w = width, h = height, g = gap;
        const half = (w - g) / 2, halfH = (h - g) / 2;
        if (n === 1)
            return Qt.rect(0, 0, w, h);
        if (n === 2)
            return Qt.rect(i * (half + g), 0, half, h);
        if (n === 3)
            return i < 2 ? Qt.rect(i * (half + g), 0, half, halfH) : Qt.rect(0, halfH + g, w, halfH);
        return Qt.rect((i % 2) * (half + g), Math.floor(i / 2) * (halfH + g), half, halfH);
    }

    Repeater {
        model: Backend.view
        delegate: CampaignPanel {
            required property string modelData
            required property int index
            readonly property rect r: grid.rect(index, Backend.view.length)
            name: modelData
            x: r.x; y: r.y; width: r.width; height: r.height
        }
    }
}
