# Notify

A small draggable widget for Claude Code on Windows. It shows session, weekly and context usage, alerts you when Claude needs input, and lets you answer from the widget.

## Run

```
cargo run --release
```

Notify listens on `127.0.0.1:47615`. The first start creates a secret token in `%LOCALAPPDATA%\notify\token`. Quit it from the tray icon.

## Connect Claude Code

Notify only sees what Claude Code sends it. Add this to `~/.claude/settings.json`, replacing `<TOKEN>` with the contents of the token file:

```json
"statusLine": {
  "type": "command",
  "command": "curl.exe -s -m 1 -H 'Expect:' --data-binary '@-' http://127.0.0.1:47615/<TOKEN>/status"
},
"hooks": {
  "PermissionRequest": [
    { "hooks": [{ "type": "http", "url": "http://127.0.0.1:47615/<TOKEN>/hook", "timeout": 120 }] }
  ],
  "Stop": [
    { "hooks": [{ "type": "http", "url": "http://127.0.0.1:47615/<TOKEN>/hook", "timeout": 120 }] }
  ],
  "UserPromptSubmit": [
    { "hooks": [{ "type": "http", "url": "http://127.0.0.1:47615/<TOKEN>/hook", "timeout": 5 }] }
  ],
  "Notification": [
    {
      "matcher": "idle_prompt|elicitation_dialog|agent_needs_input",
      "hooks": [{ "type": "http", "url": "http://127.0.0.1:47615/<TOKEN>/hook", "timeout": 5 }]
    }
  ]
}
```

- `statusLine` feeds usage: `rate_limits.five_hour`, `rate_limits.seven_day` and `context_window`. Rate limits exist only for Pro and Max, and only after the first response.
- `PermissionRequest` shows Allow and Deny. The terminal prompt stays open, so either one answers.
- `Stop` shows a reply field for 8 seconds. Typing a reply keeps Claude working with your text.
- `UserPromptSubmit` tells the ring that Claude is working. `Stop` clears it.
- `Notification` shows a short info strip.

If Notify is not running, every hook fails without blocking Claude Code.

## Undo

Remove the `statusLine` and `hooks` entries above from `settings.json`.

## Use

- Drag anywhere that is not a button. It snaps to screen edges and remembers its position.
- Hover a bar to see when it resets.
- Double-click the bars or press the arrow to collapse to rings. Click them to expand. Outer to inner: session, week, context. The centre spins while Claude works.
- Alerts expand the widget automatically. The cross dismisses an alert and leaves the answer to the terminal.
