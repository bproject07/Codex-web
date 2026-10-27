import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@xterm/xterm/css/xterm.css";
import "./styles/app.css";
import { App } from "./App";
import { GuestApp } from "./sharing/GuestApp";
import "./sharing/sharing.css";

const root = document.getElementById("root");
if (!root) {
  throw new Error("Application root element was not found");
}

createRoot(root).render(
  <StrictMode>
    {window.location.pathname === "/share" ? <GuestApp /> : <App />}
  </StrictMode>,
);

