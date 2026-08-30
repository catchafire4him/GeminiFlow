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

Copy the contents of the plugin folder into Stream Deck's plugin directory,
then restart the Stream Deck app. The same command works for a first install
and for every update afterwards:

```powershell
Copy-Item -Recurse -Force "C:\Coding\GeminiFlow_v2\streamdeck\com.geminiflow.control.sdPlugin\*" "$env:APPDATA\Elgato\StreamDeck\Plugins\com.geminiflow.control.sdPlugin\"
```

The trailing `\*` matters. Without it the command copies the folder *into*
itself on the second run and fails, because the destination already exists.

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

Stream Deck does not play animation written inside a drawing -- it renders one
still frame -- and it does not accept animated image files through the plugin
interface at all. Tested on 29 August 2026.

So the plugin draws the frames itself, sending a new picture ten times a
second, which is the rate Elgato asks plugins to stay within. The timer only
runs while something is actually moving, so an idle button costs nothing.

## Status

The half that talks to GeminiFlow is tested and working. The half that talks
to Stream Deck is written but has not been run yet — that needs the plugin
actually loaded.

Only the note button exists so far. Dictation, calls, the touch strip and the
dial come next, once this proves out.
