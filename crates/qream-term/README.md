# QREAM pane core

Standalone Rust library; no server, socket, model, account policy or keystroke automation.
A session owns `Pane`. Subscribers hold screen state only, so attaching, scrolling,
resizing a viewer or dropping it does not write input, resize the PTY or stop its child.
Dropping the owner ends its child; a dedicated waiter reaps the process. Retain the
owner for the entire session lifetime. Session journals and layout belong to T04.

`PaneSpec` takes argv verbatim, a cwd, environment overrides and initial dimensions.
Inherited environment is preserved; no approval, authentication or account flags are
added. `write` is explicit raw input; `key`, `paste`, and `mouse` encode explicit user
input using the pane's current parsed modes. None examines or answers prompts.

`subscribe(cols, rows)` is size-neutral. `Viewer::set_size` changes only its crop/padding.
Only `Pane::resize` changes the shared PTY and parser; the session layer arbitrates
which viewer may request that operation. Dimensions are bounded to 1..=4096.

`next_update` polls the current frame and coalesces rows against the last delivered
frame; there is no growing per-viewer event queue. It always includes cursor, history
length and modes. The first frame and changed viewer dimensions set `reset`.
`full` returns a complete frame and establishes the damage baseline. A newly attached
viewer receives complete replay without changing the child. The daemon supplies pane
and viewer identities when wrapping these rows in the CONTRACTS §2 event envelope.

`scroll(n)` sets a private distance into history. Nonzero positions anchor retained
rows rather than a distance from the advancing live bottom. `offset()` therefore
increases as new output scrolls. Zero follows live output. An evicted anchor clamps
to the oldest retained row; RIS resets history and releases stale anchors. Alternate
screen output is shown live, preserving the primary anchor for the return.

The vt100 parser owns a bounded 10,000-row styled ring. A nonzero internal offset is
used only as a scroll counter, restored under the parser lock after rendering. It is
never an operator's viewport. Indexed/RGB colours and inverse default colours remain
symbolic for the window palette. Runs skip wide-cell continuations; combining marks
stay with their glyph. A wide glyph clipped at the view edge is blanked.

`text(n)` returns the last n retained physical rows, stripping right padding and
unused trailing blank rows. `child_pids` reads procfs for the root and all thread
children recursively. `exited` is nonblocking. `take_error` exposes reader/wait faults.

Tests use only `/bin/sh` and standard local utilities; no harness or approval dialog
is launched. The recorded write-to-view observation includes polling, not QML paint.
