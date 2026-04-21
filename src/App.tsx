import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { load } from "@tauri-apps/plugin-store";
import { listen } from "@tauri-apps/api/event";
import { SetupView } from "./components/SetupView";
import { ChatPanel } from "./components/ChatPanel";
import { GraphCanvas } from "./components/GraphCanvas";
import { MappingWorkbench } from "./components/MappingWorkbench";
import { HomeView } from "./components/HomeView";
import "./App.css";

export type CloudProvider = 'openai' | 'gemini' | 'custom';

export type EndpointConfig = {
  base_url: string;
  api_key: string;
  label: string;
  provider?: CloudProvider | 'local' | string;
};

export type JobConfig = {
  endpoint: string;
  model: string;
  // Optional backup. Empty string = no backup.
  backup_endpoint?: string;
  backup_model?: string;
};

export type WorkspaceConfig = {
  endpoints: {
    default: EndpointConfig;
    cloud: EndpointConfig;
  };
  jobs: {
    extraction: JobConfig;
    embedding: JobConfig;
    triage_preview: JobConfig;
    evaluation: JobConfig;
    chat: JobConfig;
  };
  ui: {
    split_ratio: number;
    graph_node_cap: number;
    focus_stack_size: number;
  };
};

export type JobKey = keyof WorkspaceConfig["jobs"];

// Hard-coded provider → base URL mapping so the user never touches the URL.
export const CLOUD_PROVIDERS: Record<CloudProvider, { label: string; base_url: string; default_models: { chat: string; extraction: string; triage: string } }> = {
  openai: {
    label: 'OpenAI',
    base_url: 'https://api.openai.com/v1',
    default_models: { chat: 'gpt-4o-mini', extraction: 'gpt-4o-mini', triage: 'gpt-4o-mini' },
  },
  gemini: {
    label: 'Google Gemini',
    base_url: 'https://generativelanguage.googleapis.com/v1beta/openai',
    default_models: { chat: 'gemini-2.0-flash', extraction: 'gemini-2.0-flash', triage: 'gemini-2.0-flash' },
  },
  custom: {
    label: 'Custom (OpenAI-compatible)',
    base_url: '',
    default_models: { chat: '', extraction: '', triage: '' },
  },
};

const LOCAL_JOB_DEFAULTS: Record<JobKey, string> = {
  extraction: "qwen2.5:7b",
  embedding: "nomic-embed-text",
  triage_preview: "qwen2.5:3b",
  evaluation: "qwen2.5:7b",
  chat: "qwen2.5:7b",
};

const CLOUD_JOB_DEFAULT_KIND: Record<JobKey, "chat" | "extraction" | "triage"> = {
  extraction: "extraction",
  embedding: "extraction",
  triage_preview: "triage",
  evaluation: "extraction",
  chat: "chat",
};

export function isCloudConfigured(cfg: WorkspaceConfig | null | undefined): boolean {
  if (!cfg) return false;
  const c = cfg.endpoints.cloud;
  return Boolean(c?.base_url && c?.api_key);
}

// A concrete endpoint+model pair a job can run against. The triage model is
// derived here because the backend uses one client for both extraction and
// triage — on cloud, triage must be the cloud model too, or it 404s.
export type Target = { ep: EndpointConfig; model: string; triage: string; kind: 'default' | 'cloud' };

// Heuristics for detecting when a stored model name is obviously wrong for
// the target endpoint. Ollama uses `name:tag` slugs (qwen2.5:3b, llama3:8b);
// OpenAI/Gemini/Anthropic use flat names (gpt-4o-mini, gemini-2.0-flash).
// We use these to auto-correct stale configs rather than sending a guaranteed
// 404 to the provider.
const LOCAL_MODEL_HINTS = ['qwen', 'llama', 'mistral', 'gemma', 'phi', 'nomic', 'deepseek', 'codellama', 'tinyllama'];
const CLOUD_MODEL_HINTS = ['gpt-', 'gemini-', 'claude-', 'o1-', 'o3-', 'o4-', 'text-embedding-', 'davinci', 'babbage', 'ada'];

export function modelLooksLocal(model: string): boolean {
  if (!model) return false;
  const m = model.toLowerCase();
  if (m.includes(':')) return true; // Ollama tag format — never valid on cloud
  return LOCAL_MODEL_HINTS.some((h) => m.startsWith(h));
}

export function modelLooksCloud(model: string): boolean {
  if (!model) return false;
  const m = model.toLowerCase();
  return CLOUD_MODEL_HINTS.some((h) => m.startsWith(h));
}

export function inferCloudProvider(cloud: EndpointConfig | null | undefined): CloudProvider {
  const stored = cloud?.provider as CloudProvider | undefined;
  if (stored && stored in CLOUD_PROVIDERS) return stored;
  const url = cloud?.base_url ?? "";
  if (url.startsWith(CLOUD_PROVIDERS.openai.base_url)) return "openai";
  if (url.startsWith(CLOUD_PROVIDERS.gemini.base_url)) return "gemini";
  return url ? "custom" : "gemini";
}

export function defaultModelForJob(
  job: JobKey,
  endpoint: string,
  cfg: WorkspaceConfig | null | undefined
): string {
  const normalizedEndpoint = endpoint === "local" ? "default" : endpoint;
  if (normalizedEndpoint === "cloud") {
    const provider = inferCloudProvider(cfg?.endpoints.cloud);
    return CLOUD_PROVIDERS[provider].default_models[CLOUD_JOB_DEFAULT_KIND[job]] || "";
  }
  return LOCAL_JOB_DEFAULTS[job] || "";
}

function normalizeModelForTarget(
  job: JobKey,
  endpoint: string,
  rawModel: string,
  cfg: WorkspaceConfig | null | undefined
): string {
  const normalizedEndpoint = endpoint === "local" ? "default" : endpoint;
  const model = rawModel.trim();

  if (normalizedEndpoint === "cloud") {
    const provider = inferCloudProvider(cfg?.endpoints.cloud);
    if (!model) {
      return defaultModelForJob(job, "cloud", cfg);
    }
    if (provider !== "custom" && modelLooksLocal(model)) {
      return defaultModelForJob(job, "cloud", cfg);
    }
    return model;
  }

  if (normalizedEndpoint === "default" || normalizedEndpoint === "") {
    if (!model || modelLooksCloud(model)) {
      return defaultModelForJob(job, "default", cfg);
    }
    return model;
  }

  return model;
}

export function normalizeWorkspaceConfig(cfg: WorkspaceConfig): WorkspaceConfig {
  const normalizeJob = (jobKey: JobKey, job: JobConfig): JobConfig => {
    const endpoint = job.endpoint === "local" ? "default" : (job.endpoint || "default");
    const backupEndpoint =
      job.backup_endpoint === "none"
        ? ""
        : job.backup_endpoint === "local"
          ? "default"
          : (job.backup_endpoint || "");

    return {
      ...job,
      endpoint,
      model: normalizeModelForTarget(jobKey, endpoint, job.model || "", cfg),
      backup_endpoint: backupEndpoint,
      backup_model: backupEndpoint
        ? normalizeModelForTarget(jobKey, backupEndpoint, job.backup_model || "", cfg)
        : "",
    };
  };

  return {
    ...cfg,
    jobs: {
      extraction: normalizeJob("extraction", cfg.jobs.extraction),
      embedding: normalizeJob("embedding", cfg.jobs.embedding),
      triage_preview: normalizeJob("triage_preview", cfg.jobs.triage_preview),
      evaluation: normalizeJob("evaluation", cfg.jobs.evaluation),
      chat: normalizeJob("chat", cfg.jobs.chat),
    },
  };
}

function summarizeRoutingFailure(err: unknown): string {
  const text = String(err).replace(/\s+/g, " ").trim();
  const lower = text.toLowerCase();
  const model =
    text.match(/models\/([^"'`\s\]}]+)/i)?.[1] ??
    text.match(/model(?:s)?[/"'`\s:]+([^"'`\s,.)\]}]+)/i)?.[1];

  if ((lower.includes("http 404") || lower.includes("not found")) && model) {
    return `That endpoint doesn't have the model "${model}". Pick a model that exists on that provider in Settings -> LLM Routing.`;
  }
  if (lower.includes("http 401") || lower.includes("unauthorized") || lower.includes("invalid api key")) {
    return "The primary target rejected its API key. Check Settings -> Cloud.";
  }
  if (lower.includes("http 429") || lower.includes("rate limit") || lower.includes("quota")) {
    return "The primary target hit a rate limit or quota cap.";
  }
  if (lower.includes("connect") || lower.includes("network") || lower.includes("dns") || lower.includes("timed out")) {
    return "The primary target couldn't be reached over the network.";
  }

  return text.slice(0, 140);
}

// #region agent log
function agentLog(hypothesisId: string, location: string, message: string, data: any) {
  fetch('http://127.0.0.1:7545/ingest/a9bfa983-e081-4bef-8568-a23de219c99e', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Debug-Session-Id': 'a5604b' },
    body: JSON.stringify({
      sessionId: 'a5604b',
      runId: 'pre-fix',
      hypothesisId,
      location,
      message,
      data,
      timestamp: Date.now(),
    }),
  }).catch(() => {});
}
// #endregion

type ChatMessage = { id: string; role: 'user' | 'assistant'; text: string };

function App() {
  const [workspace, setWorkspace] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [setupMode, setSetupMode] = useState(false);
  const [graphData, setGraphData] = useState<any>(null);
  const [hasArchive, setHasArchive] = useState<boolean | null>(null);
  const [activeConversationId, setActiveConversationId] = useState<string | null>(null);
  const [selectedSidebarNode, setSelectedSidebarNode] = useState<any | null>(null);
  const [isChatLoading, setIsChatLoading] = useState(false);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [sessionSummary, setSessionSummary] = useState<string | null>(null);
  const [, setFocusStack] = useState<string[]>([]);
  const [extractionRunning, setExtractionRunning] = useState(false);
  const [extractionStopping, setExtractionStopping] = useState(false);
  const [config, setConfig] = useState<WorkspaceConfig | null>(null);
  const [extractionProgress, setExtractionProgress] = useState<{
    current: number;
    total: number;
    failed: number;
  } | null>(null);
  const [routingNotice, setRoutingNotice] = useState<string | null>(null);
  // Cache of models the cloud provider actually serves. The auto-correct
  // logic prefers this over the hard-coded preset, so stale configs
  // (e.g. `gemini-2.0-flash` on a v1beta account that doesn't serve it)
  // get swapped to a model the endpoint definitely has.
  const [cloudModels, setCloudModels] = useState<string[]>([]);
  const storeRef = useRef<any>(null);
  const extractionAbortRef = useRef<{ stopped: boolean }>({ stopped: false });
  const lastGraphRefreshAtRef = useRef<number>(0);
  const lastGraphSigRef = useRef<string>("");

  // Re-fetch the cloud provider's model list when credentials change. Filter
  // out embedding models so chat/extraction defaults never pick `text-embedding-*`.
  useEffect(() => {
    const cloud = config?.endpoints.cloud;
    if (!cloud?.base_url || !cloud?.api_key) {
      setCloudModels([]);
      return;
    }
    let cancelled = false;
    invoke<string[]>("list_available_models", {
      baseUrl: cloud.base_url,
      apiKey: cloud.api_key,
    })
      .then((m) => {
        if (cancelled) return;
        setCloudModels(m.filter((n) => !n.toLowerCase().includes("embed")));
      })
      .catch(() => {
        if (!cancelled) setCloudModels([]);
      });
    return () => {
      cancelled = true;
    };
  }, [config?.endpoints.cloud.base_url, config?.endpoints.cloud.api_key]);

  const getEndpoint = useCallback(
    (id: string): EndpointConfig | null => {
      if (id === 'cloud') {
        const c = config?.endpoints.cloud;
        return c && c.base_url && c.api_key ? c : null;
      }
      if (id === 'default' || id === 'local' || id === '') {
        return config?.endpoints.default ?? { base_url: 'http://localhost:11434/v1', api_key: '', label: 'Local' };
      }
      return null;
    },
    [config]
  );

  // Build [primary, ...fallbacks] for a job using the explicit preferred +
  // backup config. An empty backup_endpoint means "no backup": the job fails
  // hard on a preferred-endpoint error. Targets are deduped so a misconfigured
  // "backup = preferred" doesn't retry against itself.
  const resolveJobTargets = useCallback(
    (jobKey: JobKey, job: JobConfig): { targets: Target[]; notice: string | null } => {
      const cloudProvider = inferCloudProvider(config?.endpoints.cloud);

      // Live list from the provider's /models endpoint, filtered. If empty
      // (never fetched / fetch failed), we fall back to the hardcoded preset,
      // which may or may not actually exist on the account.
      const liveCloudList = cloudModels;

      // Pick a cloud model we know the provider serves. Preference order:
      //   1. The hardcoded preset if it's in the live list.
      //   2. The first live-listed model.
      //   3. The preset (last resort — may still 404 but it's our best guess).
      const cloudFallbackModel = (): string => {
        const preset = defaultModelForJob(jobKey, 'cloud', config);
        if (preset && liveCloudList.includes(preset)) return preset;
        if (liveCloudList.length > 0) return liveCloudList[0];
        return preset;
      };

      const corrections: string[] = [];
      const makeTarget = (epId: string, rawModel: string, role: 'preferred' | 'backup'): Target | null => {
        const ep = getEndpoint(epId);
        if (!ep) return null;
        const kind = epId === 'cloud' ? 'cloud' : 'default';

        // Auto-correct a model that's obviously wrong for this endpoint.
        // This is the guardrail against stale configs like "endpoint=cloud,
        // model=qwen2.5:3b" — a local Ollama tag being sent to Gemini — and
        // also against preset names the provider no longer serves.
        let model = rawModel.trim();
        if (kind === 'cloud' && !model) {
          const corrected = cloudFallbackModel();
          if (!corrected) return null;
          corrections.push(`${role} model is empty for Cloud — using "${corrected}" instead.`);
          model = corrected;
        } else if (kind === 'cloud' && cloudProvider !== 'custom' && modelLooksLocal(model)) {
          const corrected = cloudFallbackModel();
          if (!corrected) return null;
          corrections.push(
            `${role} model "${model}" isn't compatible with Cloud — using "${corrected}" instead.`
          );
          model = corrected;
        } else if (
          kind === 'cloud' &&
          cloudProvider !== 'custom' &&
          liveCloudList.length > 0 &&
          !liveCloudList.includes(model)
        ) {
          // The user's configured model isn't in the provider's /models
          // list. Substitute something that actually exists before we burn
          // a round-trip on a guaranteed 404.
          const corrected = cloudFallbackModel();
          if (corrected && corrected !== model) {
            corrections.push(
              `${role} model "${model}" isn't served by the current Cloud provider — using "${corrected}" instead.`
            );
            model = corrected;
          }
        } else if (kind === 'default' && modelLooksCloud(model)) {
          const corrected = defaultModelForJob(jobKey, 'default', config);
          corrections.push(
            `${role} model "${model}" is a cloud-only name on a Local endpoint — using "${corrected}" instead.`
          );
          model = corrected;
        } else if (kind === 'default' && !model) {
          model = defaultModelForJob(jobKey, 'default', config);
        }

        // Triage must live on the same client as extraction (backend uses one
        // client for both). On cloud, reuse the (possibly corrected) model;
        // on local, prefer the user's cheaper triage model, but if it's a
        // cloud name, fall back to a local default too.
        let triage: string;
        if (kind === 'cloud') {
          triage = model;
        } else {
          triage = normalizeModelForTarget(
            'triage_preview',
            'default',
            config?.jobs.triage_preview.model || '',
            config
          );
        }

        return { ep, model, triage, kind };
      };

      const primary = makeTarget(job.endpoint || 'default', job.model, 'preferred');
      let notice: string | null = null;
      const out: Target[] = [];

      if (primary) {
        out.push(primary);
      } else {
        // User picked cloud but hasn't configured it. Fall back silently to
        // local so the app keeps working, and tell them why.
        const localPrimary = makeTarget('default', defaultModelForJob(jobKey, 'default', config), 'preferred');
        if (localPrimary) out.push(localPrimary);
        notice = isCloudConfigured(config)
          ? "Cloud routing is selected but no runnable cloud model is set — running on the local LLM instead. Pick a cloud model in Settings -> LLM Routing to switch."
          : "Cloud endpoint isn't configured — running on the local LLM instead. Add an API key in Settings -> Cloud to switch.";
      }

      // Backup target (user-configured explicit preference). Skip if empty,
      // equal to primary's endpoint, or unreachable.
      if (job.backup_endpoint && job.backup_endpoint !== 'none') {
        const backup = makeTarget(job.backup_endpoint, job.backup_model || '', 'backup');
        if (backup && !out.some((t) => t.ep.base_url === backup.ep.base_url && t.model === backup.model)) {
          out.push(backup);
        }
      }

      // Roll up any auto-corrections into a single notice so the user knows
      // the app substituted a model — it's the difference between "silently
      // changed your config" and "protected you from a 404."
      if (corrections.length > 0) {
        const correctionNotice = `Model config auto-corrected: ${corrections.join(' ')} Update Settings → LLM Routing if you want different models.`;
        notice = notice ? `${notice} ${correctionNotice}` : correctionNotice;
      }

      return { targets: out, notice };
    },
    [config, getEndpoint, cloudModels]
  );

  const stopExtraction = useCallback(() => {
    extractionAbortRef.current.stopped = true;
    setExtractionStopping(true);
  }, []);

  // Including extractionModel in deps so Syncing triggered after a model
  // change uses the new model. (Existing workers already in flight still use
  // whichever model they started with.)
  const runExtractionWorker = useCallback(
    async (source: 'auto' | 'manual' = 'auto') => {
      if (!workspace) return;
      if (extractionRunning) return;
      extractionAbortRef.current = { stopped: false };
      setExtractionRunning(true);
      try {
        // Roll back orphan 'processing' rows from earlier stops/crashes so
        // they get retried instead of being stuck forever.
        await invoke("reset_stuck_processing", { workspacePath: workspace })
          .catch((e) => console.warn("reset_stuck_processing failed:", e));
        const pending = await invoke<string[]>("list_pending_extractions", {
          workspacePath: workspace,
        });
        agentLog("E", "src/App.tsx:runExtractionWorker", "pending_extractions_loaded", {
          source,
          workspace,
          pendingCount: pending.length,
        });
        setExtractionProgress({ current: 0, total: pending.length, failed: 0 });

        // Shared cursor + counters so N workers consume from one queue.
        let cursor = 0;
        let done = 0;
        let failed = 0;
        // Higher concurrency: Ollama on M-series chips pipelines prefill +
        // decode well, and most of each extraction is network/wait, not GPU.
        // 5 workers typically saturates Ollama before the model queue backs up.
        const CONCURRENCY = 5;

        // Build ordered targets from the user's Preferred + Backup config.
        // If nothing is configured (or cloud was picked but not set up), the
        // resolver returns a local target + a human-readable notice.
        const { targets: configuredTargets, notice: resolveNotice } = resolveJobTargets(
          'extraction',
          config?.jobs.extraction ?? { endpoint: 'default', model: LOCAL_JOB_DEFAULTS.extraction }
        );
        agentLog("E", "src/App.tsx:runExtractionWorker", "configured_targets", {
          source,
          targetCount: configuredTargets.length,
          targets: configuredTargets.map(t => ({
            kind: t.kind,
            model: t.model,
            triage: t.triage,
            base_url: t.ep?.base_url,
          })),
          resolveNotice: resolveNotice || null,
        });
        if (resolveNotice) setRoutingNotice(resolveNotice);
        if (configuredTargets.length === 0) {
          throw new Error("No runnable endpoint for extraction. Configure one in Settings.");
        }
        // Embedding always goes to Local: the workspace vec table is
        // dimensioned for 768-dim nomic-embed-text, and a cloud provider
        // wouldn't serve that model even if reachable.
        const embedEndpoint = config?.endpoints.default ?? { base_url: 'http://localhost:11434/v1', api_key: '', label: 'Local' };

        // Sticky fallback: once any worker proves the primary target is
        // broken, the rest of the run skips straight to the backup. Avoids
        // N×pending cloud timeouts when the primary is hard-down.
        let usingBackup = false;
        let fallbackNoticed = false;
        let fallbackTriggerError: string | null = null;

        const runTarget = async (t: Target, id: string) => {
          agentLog("A", "src/App.tsx:runTarget", "invoke_extract_conversation", {
            conversationId: id,
            targetKind: t.kind,
            model: t.model,
            triage: t.triage,
            baseUrl: t.ep.base_url || "http://localhost:11434/v1",
            embedBaseUrl: embedEndpoint.base_url || "http://localhost:11434/v1",
          });
          return invoke("extract_conversation", {
            conversationId: id,
            workspacePath: workspace,
            baseUrl: t.ep.base_url || "http://localhost:11434/v1",
            apiKey: t.ep.api_key || "",
            model: t.model,
            embedModel: config?.jobs.embedding.model || "nomic-embed-text",
            triageModel: t.triage,
            embedBaseUrl: embedEndpoint.base_url || "http://localhost:11434/v1",
            embedApiKey: embedEndpoint.api_key || "",
          });
        };

        const worker = async () => {
          while (true) {
            if (extractionAbortRef.current.stopped) return;
            const idx = cursor++;
            if (idx >= pending.length) return;
            const id = pending[idx];
            const startedOnBackupOnly = usingBackup && configuredTargets.length > 1;

            // If we've already proved the primary is broken, skip straight to
            // backup(s). Otherwise walk the full target list in order.
            const targets: Target[] = startedOnBackupOnly
              ? configuredTargets.slice(1)
              : configuredTargets;

            let success = false;
            const errs: string[] = [];
            for (let ti = 0; ti < targets.length; ti++) {
              const t = targets[ti];
              try {
                await runTarget(t, id);
                success = true;
                agentLog("A", "src/App.tsx:worker", "extraction_succeeded", {
                  conversationId: id,
                  targetKind: t.kind,
                  model: t.model,
                });
                break;
              } catch (e) {
                agentLog("B", "src/App.tsx:worker", "extraction_target_failed", {
                  conversationId: id,
                  targetKind: t.kind,
                  model: t.model,
                  err: String(e).slice(0, 500),
                  isPrimary: !usingBackup && ti === 0,
                });
                errs.push(`${t.kind}(${t.model}): ${String(e).slice(0, 300)}`);
                // First primary-target failure flips the rest of the run to
                // backup, and surfaces *why* once (not per-conversation).
                const isPrimary = !usingBackup && ti === 0;
                if (isPrimary && configuredTargets.length > 1) {
                  usingBackup = true;
                  fallbackTriggerError = `${t.kind}(${t.model}): ${String(e).slice(0, 300)}`;
                  if (!fallbackNoticed) {
                    fallbackNoticed = true;
                    setRoutingNotice(
                      `Primary extraction target failed — switching the rest of this sync to your backup. ${summarizeRoutingFailure(e)}`
                    );
                    agentLog("C", "src/App.tsx:worker", "switched_to_backup", {
                      conversationId: id,
                      err: String(e).slice(0, 500),
                    });
                  }
                }
              }
            }

            if (success) {
              done++;
            } else {
              failed++;
              done++;
              const combinedParts: string[] = [];
              if (startedOnBackupOnly && fallbackTriggerError) {
                combinedParts.push(
                  `Primary target failed earlier in this sync, so this conversation ran on backup only. ${fallbackTriggerError}`
                );
              }
              if (errs.length > 0) {
                combinedParts.push(errs.join(' | '));
              }
              const combined = `All endpoints failed. ${combinedParts.join(' | ')}`;
              console.warn(`[extraction ${source}] ${id} failed on all targets: ${combined}`);
              // Persist the combined error so the Failed Extractions list
              // shows the original primary-target failure that triggered
              // sticky fallback, not just the later backup-only error.
              await invoke("record_extraction_failure", {
                workspacePath: workspace,
                conversationId: id,
                error: combined,
              }).catch((recErr) => {
                console.warn("record_extraction_failure failed:", recErr);
              });
            }
            setExtractionProgress({
              current: done,
              total: pending.length,
              failed,
            });

            // Incremental graph refresh: long syncs otherwise "look stuck" even
            // though backend commits are happening. Throttle to at most once
            // every ~2 seconds and only every few completed items.
            if (workspace && !setupMode && (done % 5 === 0 || done === pending.length)) {
              const now = Date.now();
              if (now - lastGraphRefreshAtRef.current > 2000) {
                lastGraphRefreshAtRef.current = now;
                console.info(`[graph-refresh] incremental refresh at done=${done}/${pending.length}`);
                invoke("get_initial_graph", { workspacePath: workspace })
                  .then((data: any) => {
                    if (data && data.nodes && data.nodes.length > 0) {
                      setHasArchive(true);
                      setGraphData(data);
                    }
                  })
                  .catch((e) => console.warn("[graph-refresh] incremental refresh failed:", e));
              }
            }
          }
        };

        await Promise.all(
          Array.from({ length: Math.min(CONCURRENCY, pending.length) }, () =>
            worker()
          )
        );
      } catch (e) {
        console.error("Extraction worker error:", e);
      } finally {
        setExtractionRunning(false);
        setExtractionStopping(false);
        // Refresh the visible graph snapshot after extraction commits so the
        // UI reflects newly created nodes/edges without requiring a restart.
        if (workspace && !setupMode) {
          console.info("[graph-refresh] final refresh after extraction worker finished");
          invoke("get_initial_graph", { workspacePath: workspace })
            .then((data: any) => {
              if (data && data.nodes && data.nodes.length > 0) {
                setHasArchive(true);
                setGraphData(data);
              }
            })
            .catch((e) => console.error("get_initial_graph refresh failed:", e));
        }
      }
    },
    [workspace, extractionRunning, config, resolveJobTargets]
  );

  useEffect(() => {
    async function loadWorkspace() {
      try {
        const store = await load("settings.json");
        storeRef.current = store;
        const savedWorkspace = await store.get<string>("workspace_dir");
        if (savedWorkspace) {
          setWorkspace(savedWorkspace);
          try {
            const cfg = await invoke<WorkspaceConfig>("get_workspace_config", { workspacePath: savedWorkspace });
            const normalized = normalizeWorkspaceConfig(cfg);
            setConfig(normalized);
            if (JSON.stringify(normalized) !== JSON.stringify(cfg)) {
              await invoke("update_workspace_config", { workspacePath: savedWorkspace, config: normalized });
            }
          } catch(e) {
            console.error("Failed to load workspace.yaml:", e);
          }
        }
      } catch (e) {
        console.error("Failed to load settings store:", e);
      } finally {
        setLoading(false);
      }
    }
    loadWorkspace();
  }, []);

  const updateConfig = useCallback(async (newConfig: WorkspaceConfig) => {
    const normalized = normalizeWorkspaceConfig(newConfig);
    setConfig(normalized);
    if (workspace) {
      await invoke("update_workspace_config", { workspacePath: workspace, config: normalized });
    }
  }, [workspace]);

  useEffect(() => {
    if (workspace && !setupMode) {
      // Check database for initial graph
      invoke("get_initial_graph", { workspacePath: workspace })
        .then((data: any) => {
          if (!data || data.nodes.length === 0) {
            setHasArchive(false);
          } else {
            setHasArchive(true);
            lastGraphSigRef.current = `${data.nodes?.length ?? 0}:${data.links?.length ?? 0}`;
            setGraphData(data);
          }
        })
        .catch((e) => {
          console.error(e);
          setHasArchive(false);
        });
    }
  }, [workspace, setupMode]);

  // While extraction is running, poll for graph updates so newly committed
  // nodes/edges appear without waiting for a full page reload or worker finish.
  useEffect(() => {
    if (!workspace || setupMode || !extractionRunning) return;
    const id = setInterval(() => {
      invoke("get_initial_graph", { workspacePath: workspace })
        .then((data: any) => {
          if (data && data.nodes && data.nodes.length > 0) {
            const sig = `${data.nodes?.length ?? 0}:${data.links?.length ?? 0}`;
            if (sig === lastGraphSigRef.current) return;
            lastGraphSigRef.current = sig;
            setHasArchive(true);
            setGraphData(data);
          }
        })
        .catch(() => {});
    }, 2500);
    return () => clearInterval(id);
  }, [workspace, setupMode, extractionRunning]);

  useEffect(() => {
    if (workspace && hasArchive === false) {
      const unlisten = listen("tauri://file-drop", async (event: any) => {
        const paths: string[] = event.payload;
        if (paths.length > 0 && paths[0].endsWith(".json")) {
          setLoading(true);
          try {
            await invoke("ingest_conversations", {
              filePath: paths[0],
              workspacePath: workspace,
            });
            const result: any = await invoke("get_initial_graph", {
              workspacePath: workspace,
            });
            if (result && result.nodes.length > 0) {
              setHasArchive(true);
              setGraphData(result);
            }
            // Auto-start extraction now that ingest populated extraction_state.
            runExtractionWorker("auto");
          } catch (e) {
            alert(`Drop ingest failed: ${e}`);
          }
          setLoading(false);
        }
      });
      return () => {
        unlisten.then((f) => f());
      };
    }
  }, [workspace, hasArchive, runExtractionWorker]);

  // Resume-on-launch: if a workspace has conversations with extraction_state
  // still pending (previous run interrupted, or ingest happened but extraction
  // never finished), start the worker once on mount.
  useEffect(() => {
    if (!workspace || hasArchive !== true || extractionRunning) return;
    let cancelled = false;
    invoke<string[]>("list_pending_extractions", { workspacePath: workspace })
      .then((pending) => {
        if (!cancelled && pending.length > 0) {
          runExtractionWorker("auto");
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [workspace, hasArchive, extractionRunning, runExtractionWorker]);

  async function handleSelectWorkspace() {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "Select Workspace Folder",
      });

      if (selected && !Array.isArray(selected)) {
        setLoading(true);
        const path = await invoke<string>("init_workspace", { path: selected });
        if (storeRef.current) {
          await storeRef.current.set("workspace_dir", path);
          await storeRef.current.save();
        }
        setWorkspace(path);
        // Don't auto-push the user into Settings — land on the main view and
        // let the empty-archive screen guide them (import JSON / configure LLMs).
        try {
          const cfg = await invoke<WorkspaceConfig>("get_workspace_config", { workspacePath: path });
          const normalized = normalizeWorkspaceConfig(cfg);
          setConfig(normalized);
          if (JSON.stringify(normalized) !== JSON.stringify(cfg)) {
            await invoke("update_workspace_config", { workspacePath: path, config: normalized });
          }
        } catch (e) {
          console.error("Failed to load workspace.yaml:", e);
        }
        setLoading(false);
      }
    } catch (e) {
      console.error(e);
      alert("Failed to initialize workspace.");
      setLoading(false);
    }
  }

  if (loading) {
    return <div className="flex h-screen w-full bg-surface text-text"></div>;
  }

  if (!workspace) {
    return (
      <div className="flex flex-col items-center justify-center h-screen w-full bg-surface text-text gap-6">
        <div className="flex items-center gap-4 mb-4">
          <img src="/cairn-mark.svg" alt="Cairn" className="w-16 h-16" />
          <h1 className="text-3xl font-semibold tracking-tight text-text">Cairn</h1>
        </div>
        <p className="text-text-muted mb-4 max-w-md text-center">
          Choose a folder for your archive.
        </p>
        <button
          onClick={handleSelectWorkspace}
          className="bg-accent hover:opacity-90 text-surface px-6 py-2 rounded transition-colors font-medium shadow-sm"
        >
          Choose folder
        </button>
      </div>
    );
  }

  if (setupMode) {
    return (
      <div className="relative h-screen w-full bg-surface text-text">
        <button 
          className="absolute top-4 right-4 bg-stone-1 hover:bg-stone-2 text-text text-sm px-4 py-1 rounded transition-colors z-20"
          onClick={() => setSetupMode(false)}
        >
          Exit Setup
        </button>
        <SetupView
          workspace={workspace}
          onSyncAll={() => runExtractionWorker("manual")}
          onStop={stopExtraction}
          onChangeWorkspace={handleSelectWorkspace}
          extractionRunning={extractionRunning}
          extractionStopping={extractionStopping}
          extractionProgress={extractionProgress}
          config={config}
          onUpdateConfig={updateConfig}
        />
      </div>
    );
  }

  if (hasArchive === false) {
    return (
      <div className="flex flex-col items-center justify-center h-screen w-full bg-surface text-text relative">
        <div className="absolute top-4 right-4 z-20">
          <button
            className="rounded border border-stone-2 flex items-center justify-center text-text-muted bg-surface-elev hover:bg-stone-1 text-xs px-2 py-1 transition-colors"
            onClick={() => setSetupMode(true)}
          >
            Settings
          </button>
        </div>
        <div className="flex flex-col items-center gap-4 mb-2">
          <img src="/cairn-mark.svg" alt="Cairn" className="w-16 h-16 opacity-50 grayscale transition-all hover:grayscale-0" />
          <h1 className="text-2xl font-semibold tracking-tight text-text">Cairn</h1>
        </div>
        <p className="text-text-muted mb-6 max-w-md text-center">
          Drop your ChatGPT <code className="font-mono bg-stone-1 px-1 rounded text-xs">conversations.json</code> anywhere on this window to start — or open Settings to import it manually and pick which LLMs should do the work.
        </p>
        <div className="flex gap-3">
          <button
            onClick={() => setSetupMode(true)}
            className="bg-accent hover:opacity-90 text-surface text-sm px-5 py-2 rounded font-medium transition-opacity"
          >
            Open Settings
          </button>
          <button
            onClick={handleSelectWorkspace}
            className="bg-surface-elev hover:bg-stone-1 border border-stone-2 text-text text-sm px-5 py-2 rounded font-medium transition-colors"
          >
            Change workspace
          </button>
        </div>
      </div>
    );
  }

  const handleNewChat = () => {
    setMessages([]);
    setSessionSummary(null);
  };

  // §10 Session memory. Active frame = graph node IDs on screen. Recent turns
  // = last 5 Q/A pairs. Summary compresses older turns so pronouns resolve.
  const activeFrame: string[] = (graphData?.nodes ?? []).map((n: any) => n.id);

  const detectTopicShift = (q: string, lastText: string) => {
    if (!lastText) return false;
    const words = (s: string) =>
      new Set(
        s
          .toLowerCase()
          .replace(/[^\p{L}\p{N}\s]/gu, " ")
          .split(/\s+/)
          .filter((w) => w.length > 3)
      );
    const a = words(q);
    const b = words(lastText);
    const overlap = [...a].filter((w) => b.has(w)).length;
    const pronoun = /\b(it|that|this|they|them|those|these)\b/i.test(q);
    return overlap === 0 && !pronoun;
  };

  const handleSearch = async (query: string) => {
    if (!workspace) return;

    const lastAssistant = [...messages].reverse().find((m) => m.role === 'assistant');
    if (lastAssistant && detectTopicShift(query, lastAssistant.text)) {
      // Topic shift clears the active frame by not passing it, but history stays.
    }

    const newMsgId = Date.now().toString();
    const userMessage: ChatMessage = { id: newMsgId, role: 'user', text: query };
    const withUser = [...messages, userMessage];
    setMessages(withUser);
    setIsChatLoading(true);

    const recentTurns = withUser
      .slice(-10)
      .map((m) => ({ role: m.role, text: m.text }));

    const { targets: chatTargets, notice: chatNotice } = resolveJobTargets(
      'chat',
      config?.jobs.chat ?? { endpoint: 'default', model: LOCAL_JOB_DEFAULTS.chat }
    );
    if (chatNotice) setRoutingNotice(chatNotice);
    if (chatTargets.length === 0) {
      setMessages((prev) => [
        ...prev,
        { id: (Date.now() + 1).toString(), role: 'assistant', text: 'No runnable endpoint for chat. Configure one in Settings.' },
      ]);
      setIsChatLoading(false);
      return;
    }
    // Embedding stays local regardless of chat endpoint — the workspace vec
    // table is 768-dim / nomic-embed-text and cloud providers don't serve
    // that model.
    const embedEndpoint = config?.endpoints.default ?? { base_url: 'http://localhost:11434/v1', api_key: '', label: 'Local' };

    const callChat = async (t: Target) =>
      invoke("ask_question", {
        query,
        workspacePath: workspace,
        baseUrl: t.ep.base_url || "http://localhost:11434/v1",
        apiKey: t.ep.api_key || "",
        model: t.model,
        embedModel: config?.jobs.embedding.model || "nomic-embed-text",
        embedBaseUrl: embedEndpoint.base_url || "http://localhost:11434/v1",
        embedApiKey: embedEndpoint.api_key || "",
        session: {
          active_frame: activeFrame,
          recent_turns: recentTurns,
          summary: sessionSummary,
        },
      });

    try {
      let result: any;
      const errs: string[] = [];
      for (let i = 0; i < chatTargets.length; i++) {
        const t = chatTargets[i];
        try {
          result = await callChat(t);
          if (i > 0) {
            setRoutingNotice(
              `Primary chat endpoint failed — answered from your backup (${t.kind === 'cloud' ? 'Cloud' : 'Local'} / ${t.model}).`
            );
          }
          break;
        } catch (err) {
          errs.push(`${t.kind === 'cloud' ? 'Cloud' : 'Local'}(${t.model}): ${String(err).slice(0, 300)}`);
          if (i === chatTargets.length - 1) {
            throw new Error(`All chat endpoints failed.\n• ${errs.join('\n• ')}`);
          }
        }
      }

      if (result) {
        setGraphData(result.graph);
        setMessages((prev) => [
          ...prev,
          {
            id: (Date.now() + 1).toString(),
            role: 'assistant',
            text: result.text,
          },
        ]);
      }
    } catch (e) {
      console.error("Search failed:", e);
      setMessages((prev) => [
        ...prev,
        {
          id: (Date.now() + 1).toString(),
          role: 'assistant',
          text: `Error: ${e}`,
        },
      ]);
    }
    setIsChatLoading(false);
  };

  const handleLens = async (lensCmd: string) => {
    if (!workspace) return;
    setIsChatLoading(true);
    try {
      const result: any = await invoke(lensCmd, { workspacePath: workspace });
      if (result) {
        // lens_path returns { path, strength, graph }; others return GraphData.
        const g = (result as any).graph ?? result;
        setGraphData(g);
      }
    } catch (e) {
      console.error(`${lensCmd} failed:`, e);
    }
    setIsChatLoading(false);
  };

  const handleNodeClick = (node: any) => {
    if (node.group === "conversation") {
      setActiveConversationId(node.id);
      setSelectedSidebarNode(null);
      return;
    }
    
    // Select non-conversation objects for the inspector
    setSelectedSidebarNode(node);
    
    // Focus stack is implicit: clicking a node adds it to active frame via re-query.
    setFocusStack((prev) => {
      const next = [node.id, ...prev.filter((id) => id !== node.id)];
      return next.slice(0, 4);
    });
  };

  return (
    <div className="flex flex-row h-screen w-full bg-surface text-text overflow-hidden relative">
      {routingNotice && (
        <div className="absolute top-4 left-1/2 -translate-x-1/2 z-30 max-w-lg bg-surface-elev border border-flag-amber/60 rounded px-4 py-2 shadow-lg flex items-start gap-3">
          <span className="text-xs text-text leading-relaxed">{routingNotice}</span>
          <button
            onClick={() => setRoutingNotice(null)}
            className="text-text-muted hover:text-text text-xs shrink-0"
            aria-label="Dismiss"
          >
            ×
          </button>
        </div>
      )}
      <div className="w-[60%] flex-shrink-0 z-10 shadow-sm relative border-r border-stone-1">
        <div className="absolute top-4 right-4 z-20 flex gap-2">
          <button
            className="rounded border border-stone-2 flex items-center justify-center text-text-muted bg-surface-elev hover:bg-stone-1 text-xs px-2 py-1 transition-colors"
            onClick={handleNewChat}
          >
            New Chat
          </button>
          <button
            className="rounded border border-stone-2 flex items-center justify-center text-text-muted bg-surface-elev hover:bg-stone-1 text-xs px-2 py-1 transition-colors"
            onClick={() => setSetupMode(true)}
          >
            Settings
          </button>
        </div>
        <ChatPanel
          onSearch={handleSearch}
          onLens={handleLens}
          messages={messages}
          loading={isChatLoading}
        />
      </div>
      <div className="w-[40%] overflow-hidden relative">
        {messages.length === 0 && (!graphData || (graphData.nodes ?? []).length === 0) ? (
          <HomeView
            workspace={workspace}
            extractionRunning={extractionRunning}
            onAskPrompt={() => {
              const input = document.querySelector<HTMLInputElement>('input[type="text"]');
              input?.focus();
            }}
            onLens={handleLens}
            onLoadSnapshot={async (id) => {
              try {
                const state = await invoke<string>("load_snapshot", {
                  workspacePath: workspace,
                  id: parseInt(id),
                });
                const parsed = JSON.parse(state);
                if (parsed.graph) setGraphData(parsed.graph);
              } catch (e) {
                console.error("Load snapshot failed:", e);
              }
            }}
          />
        ) : (
          <GraphCanvas data={graphData} onNodeClick={handleNodeClick} />
        )}
        
        {/* Node Inspector Sidebar */}
        {selectedSidebarNode && (
          <div className="absolute right-0 top-0 bottom-0 w-64 bg-surface-elev border-l border-stone-2 shadow-xl flex flex-col transition-transform animate-in slide-in-from-right-10 overflow-y-auto">
            <div className="p-4 border-b border-stone-1 flex items-center justify-between sticky top-0 bg-surface-elev">
              <h3 className="font-semibold text-text text-sm truncate">{selectedSidebarNode.name}</h3>
              <button 
                onClick={() => setSelectedSidebarNode(null)}
                className="text-text-muted hover:text-text rounded p-1"
              >
                ×
              </button>
            </div>
            <div className="p-4 flex flex-col gap-4">
              <div className="flex gap-2 items-center">
                <span className="text-[10px] font-medium text-surface bg-accent px-2 py-0.5 rounded uppercase tracking-wider">
                  {selectedSidebarNode.group}
                </span>
                {selectedSidebarNode.props?.tier && (
                  <span className="text-[10px] font-medium text-text-muted border border-stone-2 px-2 py-0.5 rounded tracking-wider">
                    Tier {selectedSidebarNode.props.tier}
                  </span>
                )}
              </div>
              
              {selectedSidebarNode.props?.description && (
                <div className="text-xs text-text leading-relaxed">
                  {selectedSidebarNode.props.description}
                </div>
              )}
              
              {selectedSidebarNode.props?.confidence && (
                <div className="text-[11px] text-text-muted">
                  Confidence score: <strong>{selectedSidebarNode.props.confidence}%</strong>
                </div>
              )}
              
              {!selectedSidebarNode.props?.description && (
                <div className="text-xs text-text-muted italic">
                  No extended AI description available.
                </div>
              )}
            </div>
          </div>
        )}
      </div>

      {activeConversationId && workspace && (
        <MappingWorkbench 
          conversationId={activeConversationId} 
          workspace={workspace} 
          onClose={() => setActiveConversationId(null)} 
        />
      )}
    </div>
  );
}

export default App;
