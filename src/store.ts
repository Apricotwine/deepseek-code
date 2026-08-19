import { LazyStore } from "@tauri-apps/plugin-store";
import type { Language } from "./i18n";

const STORE_PATH = "settings.json";

export interface StoredSettings {
  apiKey: string;
  workspacePath: string;
  model: string;
  thinkBudget: number;
  deepBudget: number;
  language: Language;
  timeHarness: boolean;
  useHarness: boolean;
  sandbox: string;
  goalMode: boolean;
}

const DEFAULTS: StoredSettings = {
  apiKey: "",
  workspacePath: "",
  model: "deepseek-v4-flash",
  thinkBudget: 16_000,
  deepBudget: 32_000,
  language: "zh",
  timeHarness: true,
  useHarness: false,
  sandbox: "workspace-write",
  goalMode: true,
};

export async function loadSettings(): Promise<StoredSettings> {
  try {
    const store = new LazyStore(STORE_PATH);
    const apiKey = ((await store.get("api_key")) as string) || DEFAULTS.apiKey;
    const workspacePath =
      ((await store.get("workspace_path")) as string) || DEFAULTS.workspacePath;
    const model = ((await store.get("model")) as string) || DEFAULTS.model;
    const thinkBudget = ((await store.get("think_budget")) as number) || DEFAULTS.thinkBudget;
    const deepBudget = ((await store.get("deep_budget")) as number) || DEFAULTS.deepBudget;
    const language = ((await store.get("language")) as Language) || DEFAULTS.language;
    const timeHarness = ((await store.get("time_harness")) as boolean) ?? DEFAULTS.timeHarness;
    const useHarness = ((await store.get("use_harness")) as boolean) ?? DEFAULTS.useHarness;
    const sandbox = ((await store.get("sandbox")) as string) || DEFAULTS.sandbox;
    const goalMode = ((await store.get("goal_mode")) as boolean) ?? DEFAULTS.goalMode;
    return { apiKey, workspacePath, model, thinkBudget, deepBudget, language, timeHarness, useHarness, sandbox, goalMode };
  } catch {
    return DEFAULTS;
  }
}

export async function saveSettings(settings: StoredSettings): Promise<void> {
  const store = new LazyStore(STORE_PATH);
  await store.set("api_key", settings.apiKey);
  await store.set("workspace_path", settings.workspacePath);
  await store.set("model", settings.model);
  await store.set("think_budget", settings.thinkBudget);
  await store.set("deep_budget", settings.deepBudget);
  await store.set("language", settings.language);
  await store.set("time_harness", settings.timeHarness);
  await store.set("use_harness", settings.useHarness);
  await store.set("sandbox", settings.sandbox);
  await store.save();
}
