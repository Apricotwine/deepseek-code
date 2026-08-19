import type { HarnessMode } from "./types";

export interface ToolInfo {
  name: string;
  desc: string;
}

const COMMON: ToolInfo[] = [
  { name: "bash", desc: "在持久 shell 中执行命令" },
  { name: "read", desc: "读取文件内容" },
  { name: "write", desc: "写入/覆盖文件" },
  { name: "edit", desc: "对文件做精确替换" },
  { name: "grep", desc: "按模式搜索代码" },
  { name: "glob", desc: "按通配符查找文件" },
  { name: "web_search", desc: "搜索网页" },
  { name: "web_fetch", desc: "抓取并读取网页" },
  { name: "subagent", desc: "派生子代理执行任务" },
  { name: "workflow", desc: "用 parallel/pipeline 编排子任务" },
  { name: "todo_write", desc: "维护任务清单" },
  { name: "create_goal", desc: "创建目标" },
  { name: "update_goal", desc: "更新目标状态" },
  { name: "get_goal", desc: "读取当前目标" },
  { name: "skill", desc: "加载技能" },
  { name: "terminal_open", desc: "打开持久终端" },
];

export const TOOL_CATALOG: Record<HarnessMode, ToolInfo[]> = {
  standard: COMMON,
  minimal: [
    { name: "bash", desc: "在持久 shell 中执行命令" },
    { name: "str_replace_editor", desc: "查看/创建/替换/插入文件内容" },
  ],
  ptc: [
    ...COMMON,
    { name: "run_code", desc: "程序化工具调用：模型写代码批量调用工具" },
  ],
  creative: [
    ...COMMON,
    { name: "cordis_inspect_list", desc: "检查当前运行时的插件（自改运行时，待接入）" },
  ],
  ralph: [
    ...COMMON,
    { name: "ralph", desc: "多智能体按轮次接力完成长任务" },
  ],
};
