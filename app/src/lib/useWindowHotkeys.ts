import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Settings } from "./api";

/// Maps the settings value to a KeyboardEvent.code.
function dictationCode(hotkey: string | undefined): string {
  switch (hotkey) {
    case "RightAlt":
      return "AltRight";
    case "RightShift":
      return "ShiftRight";
    case "F24":
      return "F24";
    default:
      return "ControlRight";
  }
}

function matchesCallChord(e: KeyboardEvent, hotkey: string | undefined): boolean {
  switch (hotkey) {
    case "CtrlShiftK":
      return e.ctrlKey && e.shiftKey && e.code === "KeyK";
    case "CtrlShiftSemicolon":
      return e.ctrlKey && e.shiftKey && e.code === "Semicolon";
    case "F22":
      return e.code === "F22";
    default:
      return e.ctrlKey && e.shiftKey && e.code === "Quote";
  }
}

function matchesNotesChord(e: KeyboardEvent, hotkey: string | undefined): boolean {
  switch (hotkey) {
    case "CtrlShiftQuote":
      return e.ctrlKey && e.shiftKey && e.code === "Quote";
    case "F23":
      return e.code === "F23";
    default:
      return e.ctrlKey && e.shiftKey && e.code === "Semicolon";
  }
}

/**
 * Handles the hotkeys while GeminiFlow's own window has focus.
 *
 * The global keyboard hook receives nothing at all in that situation --
 * measured, not assumed: the hook's callback counter does not move while this
 * window is focused. The WebView does still get the keystrokes, so they are
 * forwarded to the same engine the hook feeds. Elsewhere the hook handles it
 * and this never fires.
 */
export function useWindowHotkeys(settings: Settings | null) {
  useEffect(() => {
    const code = dictationCode(settings?.hotkey);
    let held = false;

    const onKeyDown = (e: KeyboardEvent) => {
      // Notes is checked first to match the hook, so a colliding binding
      // behaves the same way in both places rather than diverging.
      if (matchesNotesChord(e, settings?.notesHotkey)) {
        e.preventDefault();
        invoke("ui_hotkey", { kind: "notesToggle" }).catch(() => {});
        return;
      }
      if (matchesCallChord(e, settings?.callHotkey)) {
        e.preventDefault();
        invoke("ui_hotkey", { kind: "callToggle" }).catch(() => {});
        return;
      }
      // `repeat` guards against auto-repeat opening a second session.
      if (e.code === code && !e.repeat && !held) {
        held = true;
        invoke("ui_hotkey", { kind: "press" }).catch(() => {});
      }
    };

    const onKeyUp = (e: KeyboardEvent) => {
      if (e.code === code && held) {
        held = false;
        invoke("ui_hotkey", { kind: "release" }).catch(() => {});
      }
    };

    // Losing focus mid-hold would otherwise leave a recording running with no
    // key-up ever arriving.
    const onBlur = () => {
      if (held) {
        held = false;
        invoke("ui_hotkey", { kind: "release" }).catch(() => {});
      }
    };

    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("blur", onBlur);
    };
  }, [settings?.hotkey, settings?.notesHotkey, settings?.callHotkey]);
}
