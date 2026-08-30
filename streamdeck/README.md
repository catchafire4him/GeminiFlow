# GeminiFlow Stream Deck plugin

A button that starts and stops a note, and shows what GeminiFlow is actually
doing while it does it.

That second half is the reason this exists. A plain Stream Deck hotkey can
already start a note — but it cannot know that the note stopped by itself when
it hit the time limit, so the button carries on showing "recording" when
nothing is. This plugin listens to the app instead of guessing.

## Before installing

GeminiFlow must have **External control** switched on, under Settings → 
External control, and must have been restarted since. The plugin finds the app
by reading `%APPDATA%\GeminiFlow\control.json`, which only exists once that
setting is on.

## Installing

Copy the plugin folder into Stream Deck's plugin directory, then restart the
Stream Deck app:

```
%APPDATA%\Elgato\StreamDeck\Plugins\com.geminiflow.control.sdPlugin
```

`node_modules` has to come with it — the plugin uses one library to talk to
Stream Deck, and there is no build step that would bundle it in.

Then drag **GeminiFlow → Note** onto a key.

## What the button does

Press it once to start a note, again to stop. Same behaviour as the keyboard
shortcut, because it goes through the same path inside the app rather than a
parallel one.

The picture changes with the app's real state:

| Look | Meaning |
| --- | --- |
| Grey, "offline" | GeminiFlow is not running, or external control is off |
| Grey, "Note" | Idle and ready |
| Red, "REC" | Recording a note |
| Purple, "CALL" | Recording a call |
| Amber, "…" | Working on it — transcribing or summarising |
| Red, "!" | Something failed; the app has the detail |

The app going away is ordinary, not an error: the plugin keeps trying to
reconnect, backing off to once every thirty seconds, and shows "offline"
meanwhile. Start GeminiFlow and the button catches up on its own.

## If it does not work

Stream Deck keeps a log per plugin, and everything this one does is written
there:

```
%APPDATA%\Elgato\StreamDeck\logs\
```

The two most likely causes are external control being off in GeminiFlow, and
`node_modules` not having been copied across.

## About the animation

Two of the states -- recording and working -- have their movement written into
the drawing itself rather than pushed to the button frame by frame. Stream Deck
will not accept an animated image file through the API, so this is the cheap
route if it works at all.

Look at the button while a note is recording. A ring should pulse outward from
the red dot, and three dots should take turns brightening while it transcribes.
If they sit still instead, Stream Deck is drawing a single frame and animation
has to be done the expensive way, by sending a new picture ten times a second.
Either answer is useful; nothing else needs to change to find out.

## Status

The half that talks to GeminiFlow is tested and working. The half that talks
to Stream Deck is written but has not been run yet — that needs the plugin
actually loaded.

Only the note button exists so far. Dictation, calls, the touch strip and the
dial come next, once this proves out.
