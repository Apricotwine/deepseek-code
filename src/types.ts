export type ThinkingMode = "non-think" | "think-high" | "think-max";

export type HarnessMode = "standard" | "minimal" | "ptc" | "creative" | "ralph";

export const HARNESS_MODES: { id: HarnessMode; label: string }[] = [
  { id: "standard", label: "标准" },
  { id: "minimal", label: "极简" },
  { id: "ptc", label: "PTC" },
  { id: "creative", label: "创造" },
  { id: "ralph", label: "Ralph" },
];

export interface SlashCommand {
  id: string;
  title: string;
  description: string;
  hint?: string; // right-side key hint, e.g. "Esc to dismiss"
  run: () => void | Promise<void>;
}

export type ModelId = "deepseek-v4-flash" | "deepseek-v4-pro";

export interface ModelInfo {
  id: ModelId;
  label: string;
  tagline: string;
  badge?: string;
}

export const MODELS: ModelInfo[] = [
  {
    id: "deepseek-v4-flash",
    label: "DeepSeek V4 Flash",
    tagline: "Agent-tuned · 284B MoE · 0731 GA",
    badge: "NEW",
  },
  {
    id: "deepseek-v4-pro",
    label: "DeepSeek V4 Pro",
    tagline: "Full-size · 1.6T MoE · 1M ctx",
  },
];

export interface Message {
  id: string;
  role: "user" | "assistant" | "system" | "tool";
  content: string;
  timestamp: number;
  thinkingContent?: string;
  toolCalls?: ToolCall[];
  tokenUsage?: TokenUsage;
}

export interface ToolCall {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
  result?: string;
  status: "pending" | "running" | "done" | "error";
}

export interface TokenUsage {
  input: number;
  output: number;
  cached: number;
  cache_hit_rate: number;
  /** USD saved by prompt-cache hits this session. */
  cache_savings?: number;
  /** Tokens spent on reasoning (thinking) rather than the answer. */
  thinking_tokens?: number;
}

export interface FileState {
  path: string;
  content: string;
  language: string;
  modified: boolean;
}

export interface AppState {
  messages: Message[];
  currentFile: FileState | null;
  projectPath: string | null;
  contextUsage: number;
  thinkingMode: ThinkingMode;
  isProcessing: boolean;
  sessionCost: number;
  sessionTokens: TokenUsage;
}

export interface SessionMeta {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  message_count: number;
}

export interface StoredMessage {
  role: string;
  content: string;
  timestamp: number;
  thinking_content?: string;
}

// ── Streaming event types ──

export type AgentStreamEvent =
  | { type: "turn_start"; thinking_mode: string }
  | { type: "thinking"; text: string }
  | { type: "tool_start"; id: string; name: string; args: string }
  | { type: "tool_done"; id: string; name: string; summary: string }
  | { type: "tool_error"; id: string; name: string; error: string }
  | { type: "text"; content: string }
  | { type: "subagent_started"; parent_session_id: string; child_session_id: string }
  | { type: "subagent_finished"; parent_session_id: string; child_session_id: string; status: string; summary: string }
  | { type: "trajectory"; event_type: string; summary: string }
  | { type: "turn_end"; finish_reason: string; token_usage: TokenUsage; context_usage: number };

export interface Subagent {
  id: string;
  parentId: string;
  status: "running" | "done" | "error";
  summary: string;
}

export interface TrajectoryEntry {
  type: string;
  summary: string;
}

export interface LiveActivity {
  id: string;
  type: "thinking" | "tool_start" | "tool_done" | "tool_error" | "text";
  timestamp: number;
  toolName?: string;
  toolId?: string;
  args?: string;
  summary?: string;
  content?: string;
  status: "running" | "done" | "error";
}
