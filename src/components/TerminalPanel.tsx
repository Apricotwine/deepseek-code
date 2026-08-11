import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "@xterm/xterm/css/xterm.css";

// Deep-sea palette, tuned to match the app's abyssal blue theme.
const OCEAN_THEME = {
  background: "#040914",
  foreground: "#dde4ff",
  cursor: "#6b84ff",
  cursorAccent: "#040914",
  selectionBackground: "#4d6bfe44",
  black: "#0a1228",
  red: "#f87171",
  green: "#4ade80",
  yellow: "#fbbf24",
  blue: "#6b84ff",
  magenta: "#ff7eb3",
  cyan: "#67e8f9",
  white: "#dde4ff",
  brightBlack: "#4e6096",
  brightRed: "#f87171",
  brightGreen: "#4ade80",
  brightYellow: "#fbbf24",
  brightBlue: "#8b9dff",
  brightMagenta: "#ff9cc9",
  brightCyan: "#8ff0ff",
  brightWhite: "#ffffff",
};

export default function TerminalPanel({ active }: { active: boolean }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  // One PTY session per panel instance; the panel stays mounted (hidden via
  // CSS) so the shell survives tab switches.
  const idRef = useRef<string>(crypto.randomUUID());
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const startedRef = useRef(false);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const term = new Terminal({
      fontFamily: '"SF Mono", "Cascadia Code", "JetBrains Mono", monospace',
      fontSize: 12,
      theme: OCEAN_THEME,
      cursorBlink: true,
      scrollback: 5000,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
    fit.fit();
    termRef.current = term;
    fitRef.current = fit;

    const id = idRef.current;

    const dataDisposable = term.onData((data) => {
      void invoke("terminal_input", { id, data }).catch(() => {});
    });

    let unlisten: UnlistenFn | null = null;
    void listen<{ id: string; data: string }>("terminal-output", (event) => {
      if (event.payload.id === id) {
        term.write(event.payload.data);
      }
    }).then((fn) => { unlisten = fn; unlistenRef.current = fn; });

    // Fit-to-frame zoom: keep the grid sized to the panel, notify the PTY.
    const observer = new ResizeObserver(() => {
      try {
        fit.fit();
        void invoke("terminal_resize", { id, cols: term.cols, rows: term.rows }).catch(() => {});
      } catch { /* hidden panel — skip */ }
    });
    observer.observe(container);

    return () => {
      observer.disconnect();
      dataDisposable.dispose();
      unlisten?.();
      unlistenRef.current = null;
      void invoke("kill_terminal", { id }).catch(() => {});
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, []);

  // Lazy-start: the PTY only spawns the first time the panel is actually
  // shown (the panel stays mounted, so the session survives tab switches).
  useEffect(() => {
    if (!active || startedRef.current) return;
    startedRef.current = true;
    const term = termRef.current;
    if (term) {
      void invoke("spawn_terminal", { id: idRef.current, cols: term.cols, rows: term.rows }).catch(() => {});
    }
  }, [active]);

  return (
    <div className="terminal-panel">
      <div ref={containerRef} className="terminal-container" />
    </div>
  );
}
