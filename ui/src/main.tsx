import "./app.css";
import { loadFonts } from "./fonts";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";

loadFonts();

const root = document.getElementById("root");
if (!root) throw new Error("no #root element");

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
