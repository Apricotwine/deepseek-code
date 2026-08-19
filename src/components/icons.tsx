export type IconName =
  | "files" | "history" | "agents" | "memory"
  | "diffs" | "tasks" | "monitor" | "tools" | "trajectory";

export function Icon({ name, size = 14 }: { name: IconName; size?: number }) {
  const p = {
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 2,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };
  switch (name) {
    case "files":
      return <svg {...p}><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" /></svg>;
    case "history":
      return <svg {...p}><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" /></svg>;
    case "agents":
      return <svg {...p}><circle cx="6" cy="6" r="2.5" /><circle cx="18" cy="6" r="2.5" /><circle cx="12" cy="18" r="2.5" /><path d="M6 8.5v3a2 2 0 0 0 2 2h2M18 8.5v3a2 2 0 0 1-2 2h-2" /></svg>;
    case "memory":
      return <svg {...p}><ellipse cx="12" cy="6" rx="7" ry="3" /><path d="M5 6v6c0 1.7 3.1 3 7 3s7-1.3 7-3V6" /><path d="M5 12v6c0 1.7 3.1 3 7 3s7-1.3 7-3v-6" /></svg>;
    case "diffs":
      return <svg {...p}><path d="M4 4h8v8H4z" /><path d="M14 12h6v8h-6z" /><path d="M7 7l2 2-2 2" /></svg>;
    case "tasks":
      return <svg {...p}><path d="M4 6l2 2 4-4" /><path d="M4 14l2 2 4-4" /><path d="M14 7h6M14 15h6" /></svg>;
    case "monitor":
      return <svg {...p}><path d="M4 20V10" /><path d="M10 20V4" /><path d="M16 20v-7" /><path d="M22 20H2" /></svg>;
    case "tools":
      return <svg {...p}><rect x="3" y="3" width="7" height="7" rx="1" /><rect x="14" y="14" width="7" height="7" rx="1" /><path d="M10 6h4M10 10h4M10 14h4M10 18h4" /></svg>;
    case "trajectory":
      return <svg {...p}><line x1="6" y1="4" x2="6" y2="20" /><circle cx="6" cy="6" r="1.5" /><circle cx="6" cy="12" r="1.5" /><circle cx="6" cy="18" r="1.5" /><path d="M10 6h8M10 12h6M10 18h8" /></svg>;
  }
}
