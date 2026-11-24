# The QREAM window
A Quickshell QML window over the engine's local socket (CONTRACTS §2); no web page, no HTTP.
Run: `qs --path ui/shell.qml`. It connects to `$XDG_RUNTIME_DIR/qream/engine.sock`.
The window and its refresh never start the engine. Only the explicit `qream daemon start` command starts it.
Environment: `QREAM_SOCKET` (another socket),
`QREAM_WINDOW_STATE` (where the view, fold and widths are kept), `QREAM_THEME_COLORS` (another colours file).
Development: `dev/run-daemon.sh` serves the engine's API on `dev/run/engine.sock` with real campaign folders
registered; `dev/run-window.sh` opens the window on it and starts no daemon.
Automation: `qs ipc --path ui/shell.qml call qream add|remove|show|capture <arg>`.
Closing the window stops nothing: the daemon and its sessions keep running; reopening restores the view.
