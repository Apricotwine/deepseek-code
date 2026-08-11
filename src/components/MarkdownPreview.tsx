import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { useI18n } from "../i18n";
import type { FileState } from "../types";

interface MarkdownPreviewProps {
  file: FileState;
  onEdit: () => void;
  onClose: () => void;
}

export default function MarkdownPreview({ file, onEdit, onClose }: MarkdownPreviewProps) {
  const { t } = useI18n();
  return (
    <div className="editor-panel editor-full md-preview">
      <div className="editor-header">
        <span className="editor-filename">{file.path}</span>
        <div className="editor-actions">
          {file.modified && <span className="file-modified">{t("editor.modified")}</span>}
          <button className="editor-close-btn" onClick={onEdit}>{t("editor.edit")}</button>
          <button className="editor-close-btn" onClick={onClose}>{t("editor.close")}</button>
        </div>
      </div>
      <div className="md-preview-body">
        <div className="md-preview-content">
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{file.content}</ReactMarkdown>
        </div>
      </div>
    </div>
  );
}
