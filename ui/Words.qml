pragma Singleton
import QtQuick
import Quickshell

// The window's own words (CONTRACTS §5, design sheet §4 and §6) and the colour of each state.
Singleton {
    readonly property var taskOrder: ["accepted", "stopped", "awaiting_review", "blocked", "gap_fill", "working", "waiting_on_job", "needs_attention"]
    readonly property var taskNames: ({
            accepted: "Accepted",
            stopped: "Stopped",
            awaiting_review: "Review",
            blocked: "Blocked",
            gap_fill: "Returned",
            working: "Working",
            waiting_on_job: "Waiting",
            needs_attention: "Attention"
        })
    readonly property var reasonNames: ({
            question: "question",
            quota: "quota",
            account: "account",
            launch: "launch",
            dialog: "dialog",
            e9: "stopped work"
        })
    readonly property var campaignNames: ({
            prepared: "Prepared",
            running: "Running",
            stalled: "Stalled",
            paused: "Paused",
            finished: "Finished",
            stopped: "Stopped"
        })
    readonly property string fifthRefused: "Four campaigns are in view: remove one first"
    readonly property string noCampaign: "No campaign yet"
    readonly property string addOne: "Add a campaign from the list to watch it"
    readonly property string lost: "Offline"
    readonly property string stale: "Stale"

    // Navigation and controls (CONTRACTS §5, sheet §6).
    // Divisions in the left panel; campaigns of "none" stand last without a heading.
    readonly property var divisions: ["Citium", "OVRIN", "Zavent", "none"]
    readonly property var tabs: ["Execution", "Graph", "Setup", "Review"]
    readonly property string settings: "Settings"
    readonly property string orchestrator: "Orchestrator"
    readonly property var controls: ["Trigger", "Interrupt", "Pause", "Kill", "Confirm", "Invoke", "Open the orchestrator's terminal", "Add to view", "Remove from view", "Release"]

    // Settings (RW-01-R09; sheet §3.9).
    readonly property string sound: "Sound"
    readonly property string soundOn: "Sound on"
    readonly property string muted: "Muted"
    readonly property string volume: "Volume"
    readonly property string theme: "Theme"
    readonly property string followDesktop: "Follow the desktop"
    readonly property string houseColours: "House colours"
    readonly property string orchestratorSide: "Orchestrator side"
    readonly property var sides: ["Left", "Right"]
    readonly property string profiles: "Profiles"
    readonly property string workers: "Workers"
    readonly property string harness: "Harness"
    readonly property string model: "Model"
    readonly property string effort: "Effort"
    readonly property string save: "Save"
    readonly property string campaignFolders: "Campaign folders"
    readonly property string noCampaignInForce: "Add a campaign to the view to set its profiles"
    readonly property string profileUnknown: "Not set"
    readonly property var kindNames: ({
            claude: "Claude Code",
            codex: "Codex",
            opencode: "OpenCode"
        })
    readonly property var effortNames: ({
            low: "Low",
            medium: "Medium",
            high: "High",
            xhigh: "Extra high"
        })

    // Header (sheet §2): the sound toggle and the size control's hover sentences.
    readonly property string muteHint: "Mute notification sounds"
    readonly property string unmuteHint: "Sounds are muted: click to hear them again"
    readonly property string latestNotices: "Latest notices"
    readonly property string refresh: "Refresh"
    readonly property string refreshHint: "Show every live terminal again"
    readonly property string smaller: "Smaller text"
    readonly property string defaultSize: "Default text size"
    readonly property string larger: "Larger text"
    readonly property string lockOn: "Text size changes reach every pane"
    readonly property string lockOff: "Text size changes reach the selected pane only"

    // Every other phrase the window authors, as written in its page (sheet §6); tools/words_check.sh
    // fails on any operator-facing literal in ui/ that is not in this file.
    readonly property var phrases: ["Wave", "of", "and", "more", "Earlier waves", "task left", "tasks left", "Chat", "Terminal", "Instruction", "Conversation", "Fit", "Play", "Speed", "From", "To", "Earlier campaigns", "Scope", "Ask the orchestrator to write the directives", "Review the directives", "Declare the close", "Declare the close first", "Checks pass", "Checks", "Refused", "Not installed", "Newest", "Updated", "Campaign name", "Closes after wave", "expected", "Prepared · Closes after wave", "row", "rows", "No waves yet", "No directives yet", "The checks refuse this campaign", "Choose a harness for every wave", "Pick rows or an outcome group in the grid to start a campaign", "Choose a campaign to replay", "Loading the replay", "No recorded activity for this campaign yet", "No conversation recorded for this task yet", "Resume a session", "Or type a session id", "No orchestrator yet: Invoke one, or use the terminal", "The conversation shows here once it has words", "Type a command", "Write to the orchestrator", "You", "the orchestrator", "Low", "Medium", "Claude Code", "Codex", "OpenCode", "All", "Filter", "Id", "Tab", "Status", "Purpose", "Group", "Campaign", "Age", "QREAM", "Citium", "OVRIN", "Zavent", "Row text", "Served row", "Unpicked row", "Thin row", "Exclusion line", "Work items", "Shared scope", "Folder", "Manifest", "refuses", "this campaign", "No evidence recorded", "This week", "Rows at ", "Rows now", "Read", "Write", "Other", "Built from", "tracker item", "tracker items", "This campaign has no tracker rows or outcome groups", "Reading the tracker items", "No words recorded", "Show fewer items", "No campaign chosen: choose one in the header to seat its orchestrator", "No campaign yet: register one to seat its orchestrator"]

    // A task's state is one word, always: a changing state never changes the width of a card or
    // header (his rule, OD-2026-10-11-10). The detail (which job, why blocked) is the hover's.
    function task(t) {
        return t ? (taskNames[t.word] || "") : "";
    }
    function taskDetail(t) {
        if (!t)
            return "";
        if (t.word === "blocked" && t.blocker)
            return reasonNames[t.blocker.kind] || t.blocker.kind;
        if (t.word === "waiting_on_job" && t.job)
            return t.job;
        return "";
    }
    function taskColour(word) {
        switch (word) {
        case "accepted":
            return Theme.green;
        case "awaiting_review":
            return Theme.cyan;
        case "blocked":
            return Theme.red;
        case "gap_fill":
            return Theme.orange;
        case "working":
        case "waiting_on_job":
            return Theme.blue;
        case "needs_attention":
            return Theme.yellow;
        default:
            return Theme.muted;
        }
    }
    // A row's status tone (design sheet 3.7): in build the Working blue; implemented and verified the Accepted green.
    // A tracker row's status, as a word with its first letter raised.
    function rowStatus(s) {
        return s ? s.charAt(0).toUpperCase() + s.slice(1) : "";
    }
    function rowTone(status) {
        switch (status) {
        case "in build":
            return Theme.blue;
        case "implemented":
        case "verified":
            return Theme.green;
        default:
            return Theme.quiet;
        }
    }
    function campaign(s) {
        if (!s)
            return "";
        const name = campaignNames[s.word] || "";
        if (s.word === "stalled")
            return name + " (" + (s.stalled_minutes || 0) + " min, " + (s.reason || "") + ")";
        return name;
    }
    function campaignColour(word) {
        switch (word) {
        case "running":
            return Theme.blue;
        case "stalled":
            return Theme.yellow;
        case "finished":
            return Theme.green;
        default:
            return Theme.muted;
        }
    }
    function hhmm(stamp) {
        const m = String(stamp || "").match(/T(\d\d:\d\d)/);
        return m ? m[1] : "";
    }
    // "Closes after wave N · k waves, m tasks left · expected HH:MM (was HH:MM)", all served by `states`.
    function closeLine(s) {
        if (!s || !s.close || !s.last_wave)
            return "";
        let line = "Closes after wave " + s.last_wave + " · " + s.waves_left + (s.waves_left === 1 ? " wave, " : " waves, ") + s.tasks_left + (s.tasks_left === 1 ? " task left" : " tasks left") + " · expected " + hhmm(s.close.expected_close);
        if (s.close.declared_close && hhmm(s.close.declared_close) !== hhmm(s.close.expected_close))
            line += " (was " + hhmm(s.close.declared_close) + ")";
        return line;
    }
    // "On: <row words> (<id>) · <group words> · <campaign>"
    function trace(row, group, campaign) {
        return "On: " + row.words + " (" + row.id + ") · " + (group && group.words ? group.words : "") + " · " + campaign;
    }
    // A task's title is the directive's first heading, shown whole; without one, the task is said in words.
    function title(t) {
        if (!t)
            return "";
        // The task id leads every title (TNN - outcome), as the directive's first heading reads.
        return t.title ? (t.task ? t.task + " - " + t.title : t.title) : "Task without a heading (" + (t.task || "") + ")";
    }
    // "<rule> refuses <row id> (<the row's first six words>): <reason in words>" (CONTRACTS §5).
    function refusal(rule, id, rowWords, reason) {
        const six = String(rowWords || "").split(/\s+/).filter(w => w.length > 0).slice(0, 6).join(" ");
        return rule + " refuses " + (id ? id + (six ? " (" + six + ")" : "") : "this campaign") + ": " + reason;
    }
    function profile(p) {
        if (!p || !p.kind)
            return profileUnknown;
        return (kindNames[p.kind] || p.kind) + " · " + p.model + " · " + (effortNames[p.effort] || p.effort);
    }
    function age(seconds) {
        return "Updated " + seconds + " s ago";
    }
}
