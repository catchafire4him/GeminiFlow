import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";

// Both windows load the same bundle, so each branch is imported dynamically
// and nothing crosses over. This matters more than it looks: the overlay's
// stylesheet forces a transparent body with !important, which would strip the
// main window's background if it were ever bundled alongside it.
const root = ReactDOM.createRoot(document.getElementById("root")!);

async function boot() {
  const label = getCurrentWindow().label;

  if (label === "overlay") {
    const { Overlay } = await import("./overlay/Overlay");
    root.render(
      <React.StrictMode>
        <Overlay />
      </React.StrictMode>
    );
    return;
  }

  // The floating button and its dismiss target share a stylesheet, kept out
  // of the main bundle for the same reason the overlay's is.
  if (label === "touch" || label === "touchTarget") {
    await import("./touch/touch.css");
    if (label === "touch") {
      const { TouchButton } = await import("./touch/TouchButton");
      root.render(<TouchButton />);
    } else {
      const { TouchTarget } = await import("./touch/TouchTarget");
      root.render(<TouchTarget />);
    }
    return;
  }

  await import("./styles.css");
  const { App } = await import("./App");
  root.render(
    <React.StrictMode>
      <App />
    </React.StrictMode>
  );
}

boot();
