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

// Provider → base URL mapping. Infrastructure only: the URL is determined by
// the provider identity (you can't call OpenAI at a Gemini URL). No model
// names live here — the only source of truth for models is Settings →
// LLM Routing, populated live from the provider's own /models endpoint.
export const CLOUD_PROVIDERS: Record<CloudProvider, { label: string; base_url: string }> = {
  openai: {
    label: 'OpenAI',
    base_url: 'https://api.openai.com/v1',
  },
  gemini: {
    label: 'Google Gemini',
    base_url: 'https://generativelanguage.googleapis.com/v1beta/openai',
  },
  custom: {
    label: 'Custom (OpenAI-compatible)',
    base_url: '',
  },
};

export function isCloudConfigured(cfg: WorkspaceConfig | null | undefined): boolean {
  if (!cfg) return false;
  const c = cfg.endpoints.cloud;
  return Boolean(c?.base_url && c?.api_key);
}

// A concrete endpoint+model pair a job can run against. Both fields are exactly
// what the user configured in Settings — no substitution, no heuristics.
export type Target = { ep: EndpointConfig; model: string; triage: string; kind: 'default' | 'cloud' };

export function inferCloudProvider(cloud: EndpointConfig | null | undefined): CloudProvider {
  const stored = cloud?.provider as CloudProvider | undefined;
  if (stored && stored in CLOUD_PROVIDERS) return stored;
  const url = cloud?.base_url ?? "";
  if (url.startsWith(CLOUD_PROVIDERS.openai.base_url)) return "openai";
  if (url.startsWith(CLOUD_PROVIDERS.gemini.base_url)) return "gemini";
  return url ? "custom" : "gemini";
}

function analyzeRoutingFailure(err: unknown): { summary: string; stickyBackup: boolean } {
  const text = String(err).replace(/\s+/g, " ").trim();
  const lower = text.toLowerCase();
  const model =
    text.match(/models\/([^"'`\s\]}]+)/i)?.[1] ??
    text.match(/model(?:s)?[/"'`\s:]+([^"'`\s,.)\]}]+)/i)?.[1];

  if ((lower.includes("http 404") || lower.includes("not found")) && model) {
    return {
      summary: `That endpoint doesn't have the model "${model}". Pick a model that exists on that provider in Settings -> LLM Routing.`,
      stickyBackup: true,
    };
  }
  if (lower.includes("http 404") || lower.includes("not found")) {
    return {
      summary: "The primary target returned 404 Not Found. Check the endpoint URL and selected model in Settings -> LLM Routing.",
      stickyBackup: true,
    };
  }
  if (
    lower.includes("http 401") ||
    lower.includes("http 403") ||
    lower.includes("unauthorized") ||
    lower.includes("invalid api key")
  ) {
    return {
      summary: "The primary target rejected its API key. Check Settings -> Cloud.",
      stickyBackup: true,
    };
  }
  if (lower.includes("http 429") || lower.includes("rate limit") || lower.includes("quota")) {
    return {
      summary: "The primary target hit a rate limit or quota cap.",
      stickyBackup: true,
    };
  }
  if (
    (lower.includes("http 400") || lower.includes("bad request")) &&
    (lower.includes("response_format") || lower.includes("json_object") || lower.includes("unsupported"))
  ) {
    return {
      summary: "The primary target rejected the requested JSON response format. Pick a compatible model or endpoint in Settings -> LLM Routing.",
      stickyBackup: true,
    };
  }
  if (
    lower.includes("error sending request for url") ||
    lower.includes("network error reaching") ||
    lower.includes("connect") ||
    lower.includes("network") ||
    lower.includes("dns") ||
    lower.includes("timed out") ||
    lower.includes("timeout") ||
    lower.includes("connection reset")
  ) {
    return {
      summary: "The primary target had a temporary network problem.",
      stickyBackup: false,
    };
  }
  if (lower.includes("foreign key constraint failed") || lower.includes("sqlite") || lower.includes("database")) {
    return {
      summary: "The workspace database write failed locally, so switching endpoints will not help.",
      stickyBackup: false,
    };
  }

  return { summary: text.slice(0, 140), stickyBackup: false };
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
  const [sidebarDetail, setSidebarDetail] = useState<any | null>(null);
  const [sidebarDetailLoading, setSidebarDetailLoading] = useState(false);
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
  const storeRef = useRef<any>(null);
  const extractionAbortRef = useRef<{ stopped: boolean }>({ stopped: false });
  const lastGraphRefreshAtRef = useRef<number>(0);
  const lastGraphSigRef = useRef<string>("");
  const lastGraphFpRef = useRef<string>("");

  const graphFingerprint = useCallback((data: any): string => {
    try {
      const nodes = Array.isArray(data?.nodes) ? data.nodes : [];
      const links = Array.isArray(data?.links) ? data.links : [];
      const nodeIds = nodes
        .map((n: any) => String(n?.id ?? ""))
        .filter(Boolean)
        .sort()
        .slice(0, 12);
      const linkIds = links
        .map((l: any) => `${String(l?.source ?? "")}->${String(l?.target ?? "")}:${String(l?.edge_type ?? "")}`)
        .filter((s: string) => s.length > 3)
        .sort()
        .slice(0, 12);
      return `n=${nodes.length}|l=${links.length}|n0=${nodeIds.join(",")}|e0=${linkIds.join(",")}`;
    } catch {
      return "fp_error";
    }
  }, []);

  const getEndpoint = useCallback(
    (id: string): EndpointConfig | null => {
      if (id === 'cloud') {
        const c = config?.endpoints.cloud;
        return c && c.base_url && c.api_key ? c : null;
      }
      if (id === 'default' || id === 'local' || id === '') {
        const d = config?.endpoints.default;
        return d && d.base_url ? d : null;
      }
      return null;
    },
    [config]
  );

  // Build [primary, ...backup] targets using *exactly* what the user selected
  // in Settings. No heuristics, no auto-substitution, no hardcoded fallbacks.
  // If an endpoint is unconfigured or a model is empty, that target is dropped
  // and a clear notice is returned for the caller to surface.
  const resolveJobTargets = useCallback(
    (_jobKey: JobKey, job: JobConfig): { targets: Target[]; notice: string | null } => {
      const makeTarget = (epId: string, rawModel: string): Target | null => {
        const ep = getEndpoint(epId);
        if (!ep) return null;
        const model = (rawModel || '').trim();
        if (!model) return null;
        const kind: 'cloud' | 'default' = epId === 'cloud' ? 'cloud' : 'default';

        // Triage shares the chat client in the backend, so it has to be a
        // model the SAME endpoint serves. On cloud, reuse the extraction
        // model (user's chosen one). On local, honor the user's triage
        // selection; empty = reuse extraction model.
        let triage: string;
        if (kind === 'cloud') {
          triage = model;
        } else {
          const configuredTriage = (config?.jobs.triage_preview.model || '').trim();
          triage = configuredTriage || model;
        }

        return { ep, model, triage, kind };
      };

      const out: Target[] = [];
      const notes: string[] = [];

      const endpointId = job.endpoint || 'default';
      const primary = makeTarget(endpointId, job.model || '');
      if (primary) {
        out.push(primary);
      } else {
        const epLabel = endpointId === 'cloud' ? 'Cloud' : 'Local';
        if (endpointId === 'cloud' && !isCloudConfigured(config)) {
          notes.push("Cloud isn't configured — add an API key in Settings → Cloud.");
        } else if (!(job.model || '').trim()) {
          notes.push(`No ${epLabel} model selected — pick one in Settings → LLM Routing.`);
        } else {
          notes.push(`${epLabel} endpoint isn't available — check Settings → ${epLabel} LLM.`);
        }
      }

      if (job.backup_endpoint && job.backup_endpoint !== 'none') {
        const backup = makeTarget(job.backup_endpoint, job.backup_model || '');
        if (backup && !out.some((t) => t.ep.base_url === backup.ep.base_url && t.model === backup.model)) {
          out.push(backup);
        } else if (!backup) {
          const epLabel = job.backup_endpoint === 'cloud' ? 'Cloud' : 'Local';
          notes.push(`Backup ${epLabel} has no model selected — pick one in Settings → LLM Routing.`);
        }
      }

      return { targets: out, notice: notes.length > 0 ? notes.join(' ') : null };
    },
    [config, getEndpoint]
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
          config?.jobs.extraction ?? { endpoint: '', model: '' }
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
          throw new Error("No runnable endpoint for extraction. Configure one in Settings → LLM Routing.");
        }
        // Embedding stays on the Local endpoint: the workspace vec table is
        // dimensioned for 768-dim nomic-embed-text, and a cloud provider
        // wouldn't serve that model. If either the Local endpoint or the
        // embedding model isn't configured, fail fast rather than substitute
        // a hardcoded URL / model name.
        const embedEndpoint = config?.endpoints.default;
        const embedModelName = (config?.jobs.embedding.model || '').trim();
        if (!embedEndpoint?.base_url) {
          throw new Error("Local endpoint isn't configured. Set it in Settings → Local LLM before syncing.");
        }
        if (!embedModelName) {
          throw new Error("No embedding model selected. Pick one in Settings → LLM Routing (Embedding).");
        }

        // Sticky fallback only after hard endpoint/config failures. Transient
        // transport misses should use the backup for the current conversation
        // only, then keep the rest of the queue on the preferred target.
        let usingBackup = false;
        let fallbackNoticed = false;
        let fallbackTriggerError: string | null = null;
        let transientFallbackNoticed = false;

        const runTarget = async (t: Target, id: string) => {
          agentLog("A", "src/App.tsx:runTarget", "invoke_extract_conversation", {
            conversationId: id,
            targetKind: t.kind,
            model: t.model,
            triage: t.triage,
            baseUrl: t.ep.base_url,
            embedBaseUrl: embedEndpoint.base_url,
          });
          return invoke("extract_conversation", {
            conversationId: id,
            workspacePath: workspace,
            baseUrl: t.ep.base_url,
            apiKey: t.ep.api_key || "",
            model: t.model,
            embedModel: embedModelName,
            triageModel: t.triage,
            embedBaseUrl: embedEndpoint.base_url,
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
                const failurePolicy = analyzeRoutingFailure(e);
                // Only hard endpoint/config failures flip the rest of the run
                // to backup. Transient network misses should fall back for the
                // current conversation only and then keep trying the primary.
                const isPrimary = !usingBackup && ti === 0;
                if (isPrimary && configuredTargets.length > 1) {
                  if (failurePolicy.stickyBackup) {
                    usingBackup = true;
                    fallbackTriggerError = `${t.kind}(${t.model}): ${String(e).slice(0, 300)}`;
                    if (!fallbackNoticed) {
                      fallbackNoticed = true;
                      setRoutingNotice(
                        `Primary extraction target failed — switching the rest of this sync to your backup. ${failurePolicy.summary}`
                      );
                      agentLog("C", "src/App.tsx:worker", "switched_to_backup", {
                        conversationId: id,
                        err: String(e).slice(0, 500),
                      });
                    }
                  } else if (!transientFallbackNoticed) {
                    transientFallbackNoticed = true;
                    setRoutingNotice(
                      `Primary extraction target failed for one conversation, so this item is trying your backup while the rest of the sync stays on the primary target. ${failurePolicy.summary}`
                    );
                    agentLog("C", "src/App.tsx:worker", "kept_primary_after_transient_failure", {
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
            setConfig(cfg);
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
    // Save the config verbatim — no normalization, no silent model substitution.
    // Whatever the user picks in Settings is what gets persisted and used.
    setConfig(newConfig);
    if (workspace) {
      await invoke("update_workspace_config", { workspacePath: workspace, config: newConfig });
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
            lastGraphFpRef.current = graphFingerprint(data);
            agentLog("H1", "src/App.tsx:initial_graph", "initial_graph_loaded", {
              sig: lastGraphSigRef.current,
              fp: lastGraphFpRef.current,
            });
            setGraphData(data);
          }
        })
        .catch((e) => {
          console.error(e);
          setHasArchive(false);
        });
    }
  }, [workspace, setupMode, graphFingerprint]);

  // While extraction is running, poll for graph updates so newly committed
  // nodes/edges appear without waiting for a full page reload or worker finish.
  useEffect(() => {
    if (!workspace || setupMode || !extractionRunning) return;
    const id = setInterval(() => {
      invoke("get_initial_graph", { workspacePath: workspace })
        .then((data: any) => {
          if (data && data.nodes && data.nodes.length > 0) {
            const sig = `${data.nodes?.length ?? 0}:${data.links?.length ?? 0}`;
            const fp = graphFingerprint(data);
            if (sig === lastGraphSigRef.current) {
              if (fp !== lastGraphFpRef.current) {
                agentLog("H1", "src/App.tsx:poll_graph", "poll_skipped_same_counts_but_fingerprint_changed", {
                  sig,
                  prevFp: lastGraphFpRef.current,
                  nextFp: fp,
                });
                lastGraphFpRef.current = fp;
              } else {
                agentLog("H2", "src/App.tsx:poll_graph", "poll_skipped_same_sig_and_fp", { sig, fp });
              }
              return;
            }
            lastGraphSigRef.current = sig;
            lastGraphFpRef.current = fp;
            agentLog("H3", "src/App.tsx:poll_graph", "poll_applied_graph_update", { sig, fp });
            setHasArchive(true);
            setGraphData(data);
          }
        })
        .catch(() => {});
    }, 2500);
    return () => clearInterval(id);
  }, [workspace, setupMode, extractionRunning, graphFingerprint]);

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

  // Fetch a node's connections whenever the sidebar target changes. MUST
  // live above all conditional early returns — React relies on a stable hook
  // count across renders, and putting this below `if (loading) return …`
  // caused a "rendered more hooks than previous render" crash that blanked
  // the whole UI.
  useEffect(() => {
    if (!selectedSidebarNode || !workspace) {
      setSidebarDetail(null);
      return;
    }
    let cancelled = false;
    setSidebarDetailLoading(true);
    setSidebarDetail(null);
    invoke("get_node_detail", { workspacePath: workspace, nodeId: selectedSidebarNode.id })
      .then((res: any) => {
        if (!cancelled) setSidebarDetail(res);
      })
      .catch((err) => {
        if (!cancelled) {
          console.warn("get_node_detail failed:", err);
          setSidebarDetail(null);
        }
      })
      .finally(() => {
        if (!cancelled) setSidebarDetailLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [selectedSidebarNode?.id, workspace]);

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
          setConfig(cfg);
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
      config?.jobs.chat ?? { endpoint: '', model: '' }
    );
    if (chatNotice) setRoutingNotice(chatNotice);
    if (chatTargets.length === 0) {
      setMessages((prev) => [
        ...prev,
        {
          id: (Date.now() + 1).toString(),
          role: 'assistant',
          text: chatNotice || 'No chat model selected. Configure one in Settings → LLM Routing (Chat).',
        },
      ]);
      setIsChatLoading(false);
      return;
    }

    // Embedding stays on the Local endpoint regardless of chat endpoint — the
    // workspace vec table is 768-dim nomic-embed-text and cloud providers
    // don't serve that model. Fail fast if the user hasn't configured it.
    const embedEndpoint = config?.endpoints.default;
    const embedModelName = (config?.jobs.embedding.model || '').trim();
    if (!embedEndpoint?.base_url || !embedModelName) {
      setMessages((prev) => [
        ...prev,
        {
          id: (Date.now() + 1).toString(),
          role: 'assistant',
          text: !embedEndpoint?.base_url
            ? 'Local endpoint isn\'t configured. Set it in Settings → Local LLM before chatting.'
            : 'No embedding model selected. Pick one in Settings → LLM Routing (Embedding).',
        },
      ]);
      setIsChatLoading(false);
      return;
    }

    const callChat = async (t: Target) =>
      invoke("ask_question", {
        query,
        workspacePath: workspace,
        baseUrl: t.ep.base_url,
        apiKey: t.ep.api_key || "",
        model: t.model,
        embedModel: embedModelName,
        embedBaseUrl: embedEndpoint.base_url,
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
        // If the lens has nothing to show, surface a clear notice instead of
        // silently blanking the canvas — otherwise these buttons feel broken
        // when the graph is simply empty (e.g. extraction hasn't finished).
        const nodeCount = Array.isArray(g?.nodes) ? g.nodes.length : 0;
        if (nodeCount === 0) {
          const lensLabel = lensCmd.replace('lens_', '');
          setRoutingNotice(
            `No ${lensLabel} data to show yet. Run Sync all in Settings so extraction can populate topics, entities, and concepts first.`
          );
        }
      }
    } catch (e) {
      console.error(`${lensCmd} failed:`, e);
      setRoutingNotice(`${lensCmd.replace('lens_', '')} failed: ${String(e).slice(0, 200)}`);
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
          <div className="absolute right-0 top-0 bottom-0 w-80 bg-surface-elev border-l border-stone-2 shadow-xl flex flex-col transition-transform animate-in slide-in-from-right-10 overflow-y-auto">
            <div className="p-4 border-b border-stone-1 flex items-center justify-between sticky top-0 bg-surface-elev z-10">
              <h3 className="font-semibold text-text text-sm truncate" title={selectedSidebarNode.name}>{selectedSidebarNode.name}</h3>
              <button
                onClick={() => setSelectedSidebarNode(null)}
                className="text-text-muted hover:text-text rounded p-1"
              >
                ×
              </button>
            </div>
            <div className="p-4 flex flex-col gap-4">
              <div className="flex gap-2 items-center flex-wrap">
                <span className="text-[10px] font-medium text-surface bg-accent px-2 py-0.5 rounded uppercase tracking-wider">
                  {sidebarDetail?.node_type || selectedSidebarNode.group}
                </span>
                {(sidebarDetail?.tier || selectedSidebarNode.props?.tier) && (
                  <span className="text-[10px] font-medium text-text-muted border border-stone-2 px-2 py-0.5 rounded tracking-wider uppercase">
                    Tier {sidebarDetail?.tier || selectedSidebarNode.props?.tier}
                  </span>
                )}
                {sidebarDetail && (
                  <span className="text-[10px] font-medium text-text-muted border border-stone-2 px-2 py-0.5 rounded tracking-wider">
                    {sidebarDetail.incoming_count + sidebarDetail.outgoing_count} connections
                  </span>
                )}
              </div>

              {(sidebarDetail?.description || selectedSidebarNode.props?.description) ? (
                <div className="text-xs text-text leading-relaxed">
                  {sidebarDetail?.description || selectedSidebarNode.props?.description}
                </div>
              ) : (
                <div className="text-xs text-text-muted italic">No extended description available.</div>
              )}

              {(sidebarDetail?.confidence || selectedSidebarNode.props?.confidence) && (
                <div className="text-[11px] text-text-muted">
                  Confidence: <strong>{((sidebarDetail?.confidence ?? selectedSidebarNode.props?.confidence) * (sidebarDetail?.confidence ? 100 : 1)).toFixed(0)}%</strong>
                </div>
              )}

              {/* Connections explorer — the missing "what touches this?" piece. */}
              <div className="flex flex-col gap-2 border-t border-stone-1 pt-3">
                <h4 className="text-[10px] font-semibold text-text uppercase tracking-wider">Connections</h4>
                {sidebarDetailLoading && <p className="text-[11px] text-text-muted italic">Loading…</p>}
                {!sidebarDetailLoading && sidebarDetail && sidebarDetail.neighbors.length === 0 && (
                  <p className="text-[11px] text-text-muted italic">No connections yet — this node is isolated in the graph.</p>
                )}
                {!sidebarDetailLoading && sidebarDetail && sidebarDetail.neighbors.length > 0 && (
                  <SidebarConnections
                    neighbors={sidebarDetail.neighbors}
                    onOpenConversation={(id) => {
                      setActiveConversationId(id);
                      setSelectedSidebarNode(null);
                    }}
                    onFocusNode={(n) => {
                      setSelectedSidebarNode({ id: n.id, name: n.name, group: n.node_type });
                    }}
                  />
                )}
              </div>
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

// Renders a node's neighbors grouped by (direction, edge type), with clickable
// rows that jump the user to the connected item. Conversations open the
// workbench; other nodes swap the inspector over to that node so the user
// can keep walking the graph without needing to find it visually.
function SidebarConnections({
  neighbors,
  onOpenConversation,
  onFocusNode,
}: {
  neighbors: Array<{ id: string; name: string; node_type: string; edge_type: string; direction: string }>;
  onOpenConversation: (id: string) => void;
  onFocusNode: (n: { id: string; name: string; node_type: string }) => void;
}) {
  // Friendlier English for the raw edge types coming from the graph schema.
  const labelFor = (edgeType: string, direction: string): string => {
    const incoming = direction === 'incoming';
    switch (edgeType) {
      case 'mentions':
        return incoming ? 'Mentioned in' : 'Mentions';
      case 'discusses':
        return incoming ? 'Discussed in' : 'Discusses';
      case 'belongs_to':
        return incoming ? 'Topic for' : 'Belongs to';
      case 'uses_pattern':
        return incoming ? 'Pattern used by' : 'Uses pattern';
      case 'relates_to':
        return 'Related';
      default:
        return edgeType.replace(/_/g, ' ');
    }
  };

  const groups = new Map<string, typeof neighbors>();
  for (const n of neighbors) {
    const key = `${n.direction}::${n.edge_type}`;
    const bucket = groups.get(key);
    if (bucket) bucket.push(n);
    else groups.set(key, [n]);
  }
  // Order: incoming (things that reference this node) first, because that's
  // the most common exploration question for an entity ("what chats are about this?").
  const ordered = Array.from(groups.entries()).sort(([a], [b]) => {
    if (a.startsWith('incoming::') && !b.startsWith('incoming::')) return -1;
    if (!a.startsWith('incoming::') && b.startsWith('incoming::')) return 1;
    return a.localeCompare(b);
  });

  return (
    <div className="flex flex-col gap-3">
      {ordered.map(([key, list]) => {
        const [dir, etype] = key.split('::');
        return (
          <div key={key} className="flex flex-col gap-1">
            <div className="text-[10px] font-medium text-text-muted uppercase tracking-wider flex items-center justify-between">
              <span>{labelFor(etype, dir)}</span>
              <span className="font-mono text-text-muted">{list.length}</span>
            </div>
            <div className="flex flex-col gap-0.5">
              {list.map((n) => (
                <button
                  key={`${n.id}-${n.direction}-${n.edge_type}`}
                  onClick={() =>
                    n.node_type === 'conversation'
                      ? onOpenConversation(n.id)
                      : onFocusNode({ id: n.id, name: n.name, node_type: n.node_type })
                  }
                  className="flex items-center gap-2 text-left text-xs text-text hover:text-accent hover:bg-stone-1 rounded px-1.5 py-1 transition-colors min-w-0"
                  title={n.name}
                >
                  <span
                    className={`w-1.5 h-1.5 rounded-full shrink-0 ${
                      n.node_type === 'conversation'
                        ? 'bg-stone-3'
                        : n.node_type === 'topic'
                          ? 'bg-accent'
                          : n.node_type === 'entity'
                            ? 'bg-flag-green'
                            : n.node_type === 'concept'
                              ? 'bg-flag-amber'
                              : 'bg-stone-2'
                    }`}
                  />
                  <span className="truncate flex-1">{n.name}</span>
                  <span className="text-[9px] text-text-muted uppercase tracking-wider shrink-0">{n.node_type}</span>
                </button>
              ))}
            </div>
          </div>
        );
      })}
    </div>
  );
}

export default App;
