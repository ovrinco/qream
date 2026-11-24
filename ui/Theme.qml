pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io

// Neutrals from the Omarchy theme's colours file; division accents fixed in every theme.
Singleton {
    id: theme

    property var colours: ({})
    readonly property string colourFile: Quickshell.env("QREAM_THEME_COLORS") || (Quickshell.env("HOME") + "/.local/state/omarchy/current/theme/colors.toml")

    function pick(key, fallback) {
        return colours[key] !== undefined ? colours[key] : fallback;
    }
    // Following the desktop reads the theme's colours and watches them; off keeps the house colours.
    readonly property bool follow: Backend.themeFollow
    onFollowChanged: follow ? colourView.reload() : colours = {}
    function parse(text) {
        if (!follow)
            return;
        const out = {};
        const lines = text.split("\n");
        for (let i = 0; i < lines.length; i++) {
            const m = lines[i].match(/^\s*([a-z_0-9]+)\s*=\s*"(#[0-9a-fA-F]{6})"/);
            if (m)
                out[m[1]] = m[2];
        }
        colours = out;
    }

    readonly property color background: pick("background", "#121214")
    readonly property color surface: pick("lighter_background", "#1f1f24")
    readonly property color sunken: pick("dark_background", "#0b0b0d")
    readonly property color foreground: pick("foreground", "#e3e1dc")
    readonly property color quiet: pick("dark_foreground", "#8e8e93")
    readonly property color border: Qt.rgba(foreground.r, foreground.g, foreground.b, 0.14)
    readonly property color hover: Qt.rgba(foreground.r, foreground.g, foreground.b, 0.07)

    readonly property color red: pick("red", "#d15f4e")
    readonly property color yellow: pick("yellow", "#d9a93c")
    readonly property color orange: pick("orange", "#c07f3a")
    readonly property color green: pick("green", "#7f8f6a")
    readonly property color cyan: pick("cyan", "#86a3a8")
    readonly property color blue: pick("blue", "#8090a8")
    readonly property color muted: pick("muted", "#8e8e93")

    // Terminal palette: the sixteen colours of the Omarchy terminal theme (by the colour names), then the xterm cube and greys.
    readonly property var palette: [background, red, green, yellow, blue, pick("magenta", "#b98a9a"), cyan, foreground, muted, pick("bright_red", "#ef9a8a"), pick("bright_green", "#a8b385"), pick("bright_yellow", "#f2cd6e"), pick("bright_blue", "#a3b2c9"), pick("bright_magenta", "#d9b3c4"), pick("bright_cyan", "#a9c6c9"), pick("bright_foreground", "#f4f1ea")]
    function indexed(n) {
        if (n < 16)
            return palette[n];
        if (n < 232) {
            const i = n - 16, step = v => v === 0 ? 0 : (55 + v * 40) / 255;
            return Qt.rgba(step(Math.floor(i / 36)), step(Math.floor(i / 6) % 6), step(i % 6), 1);
        }
        const g = (8 + (n - 232) * 10) / 255;
        return Qt.rgba(g, g, g, 1);
    }

    function rgb(c) {
        return Qt.rgba(c.rgb[0] / 255, c.rgb[1] / 255, c.rgb[2] / 255, 1);
    }
    function indexedForeground(c, bold) {
        if (c === "default" || c === "default_foreground")
            return bold ? pick("bright_foreground", "#f4f1ea") : foreground;
        if (c === "default_background")
            return sunken;
        if (c.indexed !== undefined)
            return indexed(bold && c.indexed < 8 ? c.indexed + 8 : c.indexed);
        return c.rgb ? rgb(c) : foreground;
    }
    function indexedBackground(c) {
        if (c === "default")
            return "transparent";
        if (c === "default_foreground")
            return foreground;
        if (c === "default_background")
            return sunken;
        if (c.indexed !== undefined)
            return indexed(c.indexed);
        return c.rgb ? rgb(c) : "transparent";
    }

    readonly property color citium: "#8b5cf6"
    readonly property color ovrin: "#daa520"
    readonly property color zavent: "#3b82f6"

    readonly property var families: Qt.fontFamilies()
    readonly property string font: families.indexOf("Inter") >= 0 ? "Inter" : "Noto Sans"
    readonly property string mono: families.indexOf("JetBrains Mono") >= 0 ? "JetBrains Mono" : families.indexOf("JetBrainsMono Nerd Font") >= 0 ? "JetBrainsMono Nerd Font" : "monospace"
    readonly property int small: 11
    readonly property int normal: 13
    readonly property int large: 15
    readonly property int radius: 4

    FileView {
        id: colourView
        path: theme.colourFile
        watchChanges: true
        onFileChanged: reload()
        onLoaded: theme.parse(text())
    }
}
