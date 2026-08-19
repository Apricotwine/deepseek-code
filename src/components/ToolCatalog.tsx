import type { HarnessMode } from "../types";
import { TOOL_CATALOG } from "../toolCatalog";
import { useI18n } from "../i18n";

interface Props {
  mode: HarnessMode;
}

export default function ToolCatalog({ mode }: Props) {
  const { t } = useI18n();
  const tools = TOOL_CATALOG[mode] ?? [];

  return (
    <div className="tool-catalog">
      <div className="tool-catalog-head">
        <span>{t("tools.count", { n: tools.length })}</span>
        <span className="tool-catalog-mode">{mode}</span>
      </div>
      {tools.map((tool) => (
        <div key={tool.name} className="tool-catalog-item">
          <code className="tool-name">{tool.name}</code>
          <span className="tool-desc">{tool.desc}</span>
        </div>
      ))}
    </div>
  );
}
