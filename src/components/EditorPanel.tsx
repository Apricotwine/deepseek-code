import { useEffect, useRef } from "react";
import { Compartment, EditorState } from "@codemirror/state";
import {
  EditorView,
  keymap,
  lineNumbers,
  highlightActiveLine,
} from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { oneDark } from "@codemirror/theme-one-dark";
import {
  syntaxHighlighting,
  defaultHighlightStyle,
} from "@codemirror/language";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { javascript } from "@codemirror/lang-javascript";
import { css } from "@codemirror/lang-css";
import { html } from "@codemirror/lang-html";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import type { FileState } from "../types";
import { useI18n } from "../i18n";

// The language packages were installed but never wired in — pick the parser
// from the file extension so code gets real syntax highlighting instead of
// the bare default style.
function languageFor(path: string | undefined) {
  const ext = (path || "").split(".").pop()?.toLowerCase() ?? "";
  switch (ext) {
    case "py": return python();
    case "rs": return rust();
    case "js": case "jsx": case "ts": case "tsx": case "mjs": case "cjs":
      return javascript({ jsx: ext === "jsx" || ext === "tsx", typescript: ext === "ts" || ext === "tsx" });
    case "css": return css();
    case "html": case "htm": case "svelte": case "vue": return html();
    case "json": return json();
    case "md": case "markdown": case "mdx": return markdown();
    default: return [];
  }
}

interface EditorPanelProps {
  currentFile: FileState | null;
  onFileChange: (file: FileState | null) => void;
  onClose: () => void;
  onPreview?: () => void;
}

export default function EditorPanel({
  currentFile,
  onFileChange,
  onClose,
  onPreview,
}: EditorPanelProps) {
  const { t } = useI18n();
  const editorRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  // Language support is reconfigured per file via a Compartment — the editor
  // view (and undo history) survive file switches, only the parser swaps.
  const languageCompRef = useRef(new Compartment());
  // The view is created once; this ref keeps the listener pointing at the
  // *current* file instead of the first one ever opened (stale closure).
  const currentFileRef = useRef<FileState | null>(currentFile);
  currentFileRef.current = currentFile;
  // Programmatic doc sync (file switch) must not count as a user edit.
  const syncingRef = useRef(false);

  useEffect(() => {
    if (!editorRef.current || viewRef.current) return;
    const state = EditorState.create({
      doc: currentFile?.content || "",
      extensions: [
        lineNumbers(),
        highlightActiveLine(),
        history(),
        syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
        oneDark,
        languageCompRef.current.of(languageFor(currentFile?.path)),
        keymap.of([...defaultKeymap, ...historyKeymap]),
        EditorView.updateListener.of((update) => {
          const file = currentFileRef.current;
          if (update.docChanged && file && !syncingRef.current) {
            onFileChange({
              ...file,
              content: update.state.doc.toString(),
              modified: true,
            });
          }
        }),
      ],
    });
    viewRef.current = new EditorView({ state, parent: editorRef.current });
    return () => {
      viewRef.current?.destroy();
      viewRef.current = null;
    };
  }, []);

  // Swap the language parser whenever a different file opens.
  useEffect(() => {
    if (!viewRef.current) return;
    const view = viewRef.current;
    if (currentFile) {
      syncingRef.current = true;
      view.dispatch({
        effects: languageCompRef.current.reconfigure(languageFor(currentFile.path)),
      });
      if (view.state.doc.toString() !== currentFile.content) {
        view.dispatch({
          changes: {
            from: 0,
            to: view.state.doc.length,
            insert: currentFile.content,
          },
        });
      }
      syncingRef.current = false;
    }
  }, [currentFile]);

  return (
    <div className="editor-panel editor-full">
      <div className="editor-header">
        <span className="editor-filename">{currentFile?.path || ""}</span>
        <div className="editor-actions">
          {currentFile?.modified && (
            <span className="file-modified">{t("editor.modified")}</span>
          )}
          {onPreview && (
            <button className="editor-close-btn" onClick={onPreview}>{t("editor.preview")}</button>
          )}
          <button className="editor-close-btn" onClick={onClose}>
            {t("editor.close")}
          </button>
        </div>
      </div>
      <div ref={editorRef} className="editor-container" />
    </div>
  );
}
