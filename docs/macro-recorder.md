# Macro Recorder User Guide

The Macro Recorder lets you record and replay input events in the FrankenTUI demo showcase. Use it to create reproducible demos, automate testing scenarios, or explore the UI hands-free.

## Quick Start

1. Navigate to the **Macro Recorder** screen (press `Tab` until you reach it, or use the number key shortcut)
2. Press `r` to start recording
3. Navigate through screens, press keys, interact with the UI
4. Press `r` again (or `Esc`) to stop recording
5. Press `p` to replay your recorded macro

## Recording Controls

| Key | Action | Description |
|-----|--------|-------------|
| `r` | Toggle Record | Start a new recording, or stop the current one |
| `Esc` | Stop | Stop recording and save the macro |

### What Gets Recorded

- **Keyboard events**: All key presses (except control keys listed below)
- **Mouse events**: Clicks, movement, scroll (if enabled)
- **Resize events**: Terminal size changes
- **Paste events**: Clipboard pastes
- **Timing**: Delay between each event is preserved

### What Does NOT Get Recorded

Control keys are automatically filtered to prevent recording the controls themselves:

- `r`, `p`, `l` - Recording/playback controls
- `+`, `=`, `-` - Speed adjustment
- `Esc` - Stop commands

## Playback Controls

| Key | Action | Description |
|-----|--------|-------------|
| `p` | Play/Pause | Start playback, or pause if already playing |
| `Esc` | Stop | Stop playback completely |
| `l` | Toggle Loop | Enable/disable automatic looping |
| `+` or `=` | Speed Up | Increase playback speed (+0.25x) |
| `-` | Speed Down | Decrease playback speed (-0.25x) |

### Speed Settings

- **Minimum**: 0.25x (quarter speed)
- **Default**: 1.0x (real-time)
- **Maximum**: 4.0x (4x speed)

The current speed is displayed in the control panel.

### Looping

When loop mode is enabled (`l`), playback automatically restarts from the beginning when it reaches the end. This is useful for demos that should run continuously.

## Timeline Panel

The timeline shows all recorded events in order:

```
▶ 1. Key 'g'           +0ms     (0ms)
  2. Key 'g'           +52ms    (52ms)
  3. Key 'd'           +68ms    (120ms)
○ 4. Key 'Tab'         +200ms   (320ms)
```

- `▶` marks the currently playing event
- `●` marks events that have already played
- `○` marks events yet to play
- **+Nms** shows the delay from the previous event
- **(Nms)** shows cumulative time from start

## Scenario Runner

The Scenario Runner panel lists predefined demo macros:

| Scenario | Description |
|----------|-------------|
| Tab Tour | Navigate through all screens using Tab |
| Search Flow | Demonstrate search functionality |
| Layout Lab | Explore responsive layout features |

Select a scenario and press `p` to play it.

### Creating Custom Scenarios

Scenarios are stored as recorded macros. To create a new scenario:

1. Record your desired sequence
2. Export the macro (future feature)
3. Add it to the scenario configuration

## States

The recorder operates in one of these states:

| State | Icon | Description |
|-------|------|-------------|
| Idle | `○` | Ready to record or play |
| Recording | `●` | Actively recording events |
| Stopped | `■` | Macro ready for playback |
| Playing | `▶` | Playback in progress |
| Paused | `❚❚` | Playback paused |
| Error | `⚠` | An error occurred |

## Error Recovery

If playback fails:

1. An error message appears with details
2. Press `Esc` to clear the error and return to Idle
3. The recorded macro is preserved (unless corrupted)

Common errors:

- **Empty macro**: No events were recorded
- **Invalid event**: Event data is malformed
- **Playback interrupted**: Focus lost or terminal resized

## Determinism Guarantee

Macros are designed to replay deterministically:

- Events fire in the exact recorded order
- Timing is normalized (recorded delays, not wall clock)
- Speed scaling affects timing but never event order
- Multiple replays produce identical event streams

This makes macros ideal for automated testing and reproducible demos.

## Tips

1. **Practice run**: Do a practice run before recording to plan your sequence
2. **Clean start**: Start from a known state (e.g., first screen)
3. **Pause for effect**: Natural pauses are recorded and replayed
4. **Speed up reviews**: Use 2x or 4x speed when reviewing recordings
5. **Loop for demos**: Enable looping for unattended demonstrations

## Integration with E2E Testing

Macros can be used for E2E testing:

```bash
# Run demo with a pre-recorded scenario
FTUI_DEMO_MACRO=scenarios/quick-tour.json cargo run -p ftui-demo-showcase

# Record a new scenario to a file
FTUI_DEMO_RECORD=output.json cargo run -p ftui-demo-showcase
```

See `docs/spec/macro-recorder.md` for technical details on the timing model, data format, and test requirements.

## Keyboard Shortcut Summary

| Key | Recording | Playback |
|-----|-----------|----------|
| `r` | Start/Stop recording | - |
| `p` | - | Play/Pause |
| `l` | - | Toggle loop |
| `+`/`=` | - | Speed up |
| `-` | - | Speed down |
| `Esc` | Stop recording | Stop playback |
