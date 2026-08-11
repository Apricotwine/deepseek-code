import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface FileEntry {
  name: string;
  is_dir: boolean;
}

interface SidebarProps {
  onOpenFile: (path: string) => void;
  workspacePath: string;
}

export default function Sidebar({ onOpenFile, workspacePath }: SidebarProps) {
  // dir -> entries; lazy-loaded per directory and cached while the tree is open.
  const [dirCache, setDirCache] = useState<Record<string, FileEntry[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  const loadDir = useCallback(async (dir: string) => {
    try {
      const entries = await invoke<FileEntry[]>("list_workspace_files", {
        path: dir || null,
      });
      setDirCache((prev) => ({ ...prev, [dir]: entries }));
    } catch {
      setDirCache((prev) => ({ ...prev, [dir]: [] }));
    }
  }, []);

  // Reset the tree when the workspace changes.
  useEffect(() => {
    setDirCache({});
    setExpanded(new Set());
    loadDir("");
  }, [loadDir, workspacePath]);

  const toggleDir = (path: string) => {
    if (expanded.has(path)) {
      setExpanded((prev) => {
        const next = new Set(prev);
        next.delete(path);
        return next;
      });
    } else {
      setExpanded((prev) => new Set(prev).add(path));
      if (!dirCache[path]) loadDir(path);
    }
  };

  return (
    <div className="sidebar-files" style={{ flex: 1, overflow: "auto" }}>
      <Tree
        dir=""
        dirCache={dirCache}
        expanded={expanded}
        depth={0}
        onToggleDir={toggleDir}
        onOpenFile={onOpenFile}
      />
      <div className="sidebar-footer">
        <span className="sidebar-path">{workspacePath.split("/").pop() || "workspace"}</span>
      </div>
    </div>
  );
}

function Tree({
  dir,
  dirCache,
  expanded,
  depth,
  onToggleDir,
  onOpenFile,
}: {
  dir: string;
  dirCache: Record<string, FileEntry[]>;
  expanded: Set<string>;
  depth: number;
  onToggleDir: (path: string) => void;
  onOpenFile: (path: string) => void;
}) {
  const entries = dirCache[dir];
  if (!entries) return null;

  const childPath = (name: string) => (dir ? `${dir}/${name}` : name);
  const paddingLeft = 10 + depth * 12;

  return (
    <>
        {entries.map((f) => {
        const path = childPath(f.name);
        if (f.is_dir) {
          const isOpen = expanded.has(path);
          return (
            <div key={path}>
              <div
                className="file-row dir"
                style={{ paddingLeft }}
                onClick={() => onToggleDir(path)}
              >
                <span className="file-icon">{isOpen ? "v" : ">"}</span>
                <span className="file-name">{f.name}</span>
              </div>
              {isOpen && (
                <Tree
                  dir={path}
                  dirCache={dirCache}
                  expanded={expanded}
                  depth={depth + 1}
                  onToggleDir={onToggleDir}
                  onOpenFile={onOpenFile}
                />
              )}
            </div>
          );
        }
        return (
          <div
            key={path}
            className="file-row"
            style={{ paddingLeft }}
            onClick={() => onOpenFile(path)}
          >
            <span className="file-icon-file" />
            <span className="file-name">{f.name}</span>
          </div>
        );
      })}
    </>
  );
}
