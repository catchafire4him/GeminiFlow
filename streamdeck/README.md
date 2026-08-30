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

## What you get

Four things to drag onto the deck.

**Dictate**, **Note** and **Call** are keys. Press once to start, again to
stop. They behave exactly like the keyboard shortcuts because they go through
the same path inside the app rather than a parallel one — a dictation started
from the deck is the same session as one started with Right Ctrl.

Dictation is the exception worth knowing about: at the keyboard it is
hold-to-talk, which a button press cannot express. The button sends the start
and the stop itself, deciding which one is next from what the app says it is
doing. So it is a two-press button here and a hold-to-talk key there, and both
drive the same recording.

**Status** goes on the touch strip of a Stream Deck +. It shows what the app
is doing, how long the current recording has run, and the microphone level
along the bottom. Turn its dial to change the microphone level — the same
setting the slider in Settings changes, not a second one beside it. Push the
dial or tap the strip to start or stop a note.

The strip takes one quarter of the display. A plugin cannot span the whole
thing; each quarter belongs to one dial.

## What the pictures mean

| Look | Meaning |
| --- | --- |
| Grey, "offline" | GeminiFlow is not running, or external control is off |
| Grey glyph | Idle and ready |
| Red, pulsing | Recording |
| Purple, pulsing | Recording a call |
| Amber dots | Transcribing or summarising |
| Red exclamation | Something failed; the app has the detail |

Each key only reacts to its own work. Recording a note does not light up the
dictation key — a button that lights up for something it did not start is
worse than one that stays dark.

The app going away is ordinary, not an error: the plugin keeps trying to
reconnect, backing off to once every thirty seconds, and shows "offline"
meanwhile. Start GeminiFlow and everything catches up on its own.

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

Working and in use: the control link, all three keys, the state-driven
pictures, and the frame-by-frame animation. The microphone endpoints are
verified against the running app — read 80, set 70, read back 70.

The touch strip and dial are written but unproven; they need a Stream Deck +
in front of them. If the strip stays blank, the custom layout in
`layouts/strip.json` is the first thing to suspect.
