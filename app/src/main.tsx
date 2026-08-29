import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";

// Both windows load the same bundle, so each branch is imported dynamically
// and nothing crosses over. This matters more than it looks: the overlay's
// stylesheet forces a transparent body with !important, which would strip the
// main window's background if it were ever bundled alongside it.
const root = ReactDOM.createRoot(document.getElementById("root")!);

async function boot() {
  if (getCurrentWindow().label === "overlay") {
    const { Overlay } = await import("./overlay/Overlay");
    root.render(
      <React.StrictMode>
        <Overlay />
      </React.StrictMode>
    );
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
