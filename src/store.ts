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
    const goalMode = ((await store.get("goal_mode")) as boolean) ?? DEFAULTS.goalMode;
    return { apiKey, workspacePath, model, thinkBudget, deepBudget, language, timeHarness, goalMode };
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
  await store.save();
}
