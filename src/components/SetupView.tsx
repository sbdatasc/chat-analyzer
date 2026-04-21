import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import {
  CLOUD_PROVIDERS,
  defaultModelForJob,
  inferCloudProvider,
  type CloudProvider,
  type WorkspaceConfig,
} from "../App";

export function SetupView({
  workspace,
  config,
  onUpdateConfig,
  onSyncAll,
  onStop,
  onChangeWorkspace,
  extractionRunning,
  extractionStopping,
  extractionProgress,
}: {
  workspace: string;
  config: WorkspaceConfig | null;
  onUpdateConfig: (cfg: WorkspaceConfig) => void;
  onSyncAll?: () => void;
  onStop?: () => void;
  onChangeWorkspace?: () => void;
  extractionRunning?: boolean;
  extractionStopping?: boolean;
  extractionProgress?: { current: number; total: number; failed: number } | null;
}) {
  const [localModels, setLocalModels] = useState<string[]>([]);
  const [cloudModels, setCloudModels] = useState<string[]>([]);

  useEffect(() => {
    invoke<string[]>("list_available_models", {
      baseUrl: "http://localhost:11434/v1",
      apiKey: "",
    })
      .then((m) =>
        setLocalModels(m.filter((name) => !name.toLowerCase().includes("embed")))
      )
      .catch(() => setLocalModels([]));
  }, []);

  useEffect(() => {
    if (!config?.endpoints.cloud.base_url || !config?.endpoints.cloud.api_key) return;
    invoke<string[]>("list_available_models", {
      baseUrl: config.endpoints.cloud.base_url,
      apiKey: config.endpoints.cloud.api_key,
    })
      .then((m) =>
        setCloudModels(m.filter((name) => !name.toLowerCase().includes("embed")))
      )
      .catch(() => setCloudModels([]));
  }, [config?.endpoints.cloud.base_url, config?.endpoints.cloud.api_key]);

  const [ingesting, setIngesting] = useState(false);
  const [ingestResult, setIngestResult] = useState<string | null>(null);
  const [checkingLlm, setCheckingLlm] = useState(false);
  const [healthStatus, setHealthStatus] = useState<any>(null);
  const [checkingCloud, setCheckingCloud] = useState(false);
  const [cloudHealth, setCloudHealth] = useState<any>(null);
  const [cloudHealthError, setCloudHealthError] = useState<string | null>(null);
  const [showAdvancedCloud, setShowAdvancedCloud] = useState(false);

  // Resolve which provider is currently active based on base_url, so users who
  // upgrade from an older workspace.yaml (no provider field) still see the
  // right dropdown value.
  const currentProvider: CloudProvider = useMemo(
    () => inferCloudProvider(config?.endpoints.cloud),
    [config?.endpoints.cloud]
  );
  const [status, setStatus] = useState<{
    conversations: number;
    done: number;
    pending: number;
    processing: number;
    failed: number;
    parked: number;
  } | null>(null);
  const [syncStartedAt, setSyncStartedAt] = useState<number | null>(null);
  const [failures, setFailures] = useState<
    { id: string; title: string; error: string }[]
  >([]);
  const [showFailures, setShowFailures] = useState(false);
  const [retryingAllFailed, setRetryingAllFailed] = useState(false);

  const refreshFailures = async () => {
    try {
      const rows = await invoke<
        { id: string; title: string; error: string }[]
      >("list_failed_extractions", { workspacePath: workspace });
      setFailures(rows);
    } catch (e) {
      console.warn("list_failed_extractions failed:", e);
    }
  };

  useEffect(() => {
    refreshFailures();
  }, [workspace, status?.failed]);

  const handleRetry = async (id: string) => {
    try {
      await invoke("retry_extraction", {
        workspacePath: workspace,
        conversationId: id,
      });
      setFailures((prev) => prev.filter((f) => f.id !== id));
    } catch (e) {
      alert(`Retry failed: ${e}`);
    }
  };

  const handleRetryAllFailed = async () => {
    if (retryingAllFailed || failures.length === 0) return;
    setRetryingAllFailed(true);
    try {
      const retried = await invoke<number>("retry_all_failed_extractions", {
        workspacePath: workspace,
      });
      if (retried > 0) {
        setFailures([]);
        if (onSyncAll && !extractionRunning) {
          onSyncAll();
        }
      }
    } catch (e) {
      alert(`Retry all failed: ${e}`);
    } finally {
      setRetryingAllFailed(false);
    }
  };

  const handleSkip = async (id: string) => {
    try {
      await invoke("skip_extraction", {
        workspacePath: workspace,
        conversationId: id,
      });
      setFailures((prev) => prev.filter((f) => f.id !== id));
    } catch (e) {
      alert(`Skip failed: ${e}`);
    }
  };

  useEffect(() => {
    const pull = () =>
      invoke("get_workspace_status", { workspacePath: workspace })
        .then((s: any) => setStatus(s))
        .catch(() => {});
    pull();
    const interval = setInterval(pull, extractionRunning ? 1500 : 8000);
    return () => clearInterval(interval);
  }, [workspace, extractionRunning]);

  useEffect(() => {
    if (extractionRunning && syncStartedAt === null) {
      setSyncStartedAt(Date.now());
    }
    if (!extractionRunning) {
      setSyncStartedAt(null);
    }
  }, [extractionRunning, syncStartedAt]);

  const [, tick] = useState(0);
  useEffect(() => {
    if (!extractionRunning) return;
    const id = setInterval(() => tick((t) => t + 1), 1000);
    return () => clearInterval(id);
  }, [extractionRunning]);

  async function runIngest(filePath: string, force: boolean): Promise<void> {
    const sourceId = await invoke<string>("ingest_conversations", {
      filePath,
      workspacePath: workspace,
      force,
    });
    setIngestResult(
      force
        ? `Re-imported. Source ${sourceId.substring(0, 8)}.`
        : `Imported. Source ${sourceId.substring(0, 8)}.`
    );
  }

  async function handleIngest() {
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "JSON", extensions: ["json"] }],
        title: "Select ChatGPT conversations.json",
      });

      if (selected && !Array.isArray(selected)) {
        setIngesting(true);
        setIngestResult(null);
        try {
          await runIngest(selected, false);
        } catch (e: any) {
          const msg = String(e);
          if (msg.includes("Already imported")) {
            const ok = window.confirm(
              `${msg}\n\nOverride the existing import with this file?`
            );
            if (ok) {
              try {
                await runIngest(selected, true);
              } catch (e2: any) {
                alert(`Override failed. ${e2}`);
              }
            }
          } else {
            alert(`Import failed. ${msg}`);
          }
        }
        setIngesting(false);
      }
    } catch (e) {
      console.error(e);
      alert("Failed to open file dialog.");
      setIngesting(false);
    }
  }

  async function handleReset() {
    const ok = window.confirm(
      "Reset workspace? This deletes the database and every imported conversations.json. Your workspace.yaml and the folder itself are kept. You'll need to re-import and re-extract."
    );
    if (!ok) return;
    try {
      await invoke("reset_workspace", { workspacePath: workspace });
      setIngestResult(null);
      setHealthStatus(null);
      setStatus({
        conversations: 0,
        done: 0,
        pending: 0,
        processing: 0,
        failed: 0,
        parked: 0,
      });
      alert("Workspace reset. Import a conversations.json to begin.");
    } catch (e: any) {
      alert(`Reset failed. ${e}`);
    }
  }

  async function handleCheckLlm() {
    setCheckingLlm(true);
    setHealthStatus(null);
    try {
      const s = await invoke("check_llm_health", {
        baseUrl: "http://localhost:11434/v1",
        apiKey: "ollama",
      });
      setHealthStatus(s);
    } catch (e: any) {
      // Use inline surfacing (like cloud) instead of an alert — alert() blocks
      // the page and is the wrong tone for a passive health check.
      setHealthStatus({ models: false, chat: false, embeddings: false, json_mode: false, models_error: String(e) });
    }
    setCheckingLlm(false);
  }

  // Auto-run both health checks on first entry. User can still manually
  // trigger them via the buttons; this just gives them an immediate picture
  // of what's reachable without an extra click.
  const autoCheckedRef = useRef(false);
  const cloudReady = Boolean(config?.endpoints.cloud.base_url && config?.endpoints.cloud.api_key);
  useEffect(() => {
    if (autoCheckedRef.current) return;
    autoCheckedRef.current = true;
    handleCheckLlm();
    if (cloudReady) handleCheckCloud();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // If the cloud credentials arrive after mount (e.g. user just pasted a
  // key), auto-run the cloud probe so the status updates without a click.
  useEffect(() => {
    if (!cloudReady) return;
    if (cloudHealth) return;
    handleCheckCloud();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cloudReady]);

  const handleUpdateJobModel = (job: keyof WorkspaceConfig['jobs'], endpoint: string, model: string) => {
    if (!config) return;
    onUpdateConfig({
      ...config,
      jobs: {
        ...config.jobs,
        [job]: { ...config.jobs[job], endpoint, model }
      }
    });
  };

  const handleUpdateJobBackup = (job: keyof WorkspaceConfig['jobs'], backup_endpoint: string, backup_model: string) => {
    if (!config) return;
    onUpdateConfig({
      ...config,
      jobs: {
        ...config.jobs,
        [job]: { ...config.jobs[job], backup_endpoint, backup_model }
      }
    });
  };

  const handleUpdateCloudEndpoint = (field: 'base_url' | 'api_key', val: string) => {
    if (!config) return;
    onUpdateConfig({
      ...config,
      endpoints: {
        ...config.endpoints,
        cloud: {
          ...config.endpoints.cloud,
          [field]: val
        }
      }
    });
  }

  const handleSelectProvider = (provider: CloudProvider) => {
    if (!config) return;
    const preset = CLOUD_PROVIDERS[provider];
    // Provider switch clears the API key — keys don't transfer between
    // providers, and leaving the old key in place would just fail on next call.
    const cloud = {
      ...config.endpoints.cloud,
      provider,
      label: preset.label,
      base_url: provider === 'custom' ? (config.endpoints.cloud.base_url || '') : preset.base_url,
      api_key: provider === currentProvider ? config.endpoints.cloud.api_key : '',
    };
    // Seed sensible default models for any job already routed to cloud, but
    // only if the current model is empty (don't clobber a user's explicit pick).
    const seed = (job: keyof WorkspaceConfig['jobs'], which: 'chat' | 'extraction' | 'triage') => {
      const j = config.jobs[job];
      if (j.endpoint !== 'cloud') return j;
      if (j.model && j.model.trim() !== '') return j;
      return { ...j, model: preset.default_models[which] };
    };
    onUpdateConfig({
      ...config,
      endpoints: { ...config.endpoints, cloud },
      jobs: {
        ...config.jobs,
        chat: seed('chat', 'chat'),
        extraction: seed('extraction', 'extraction'),
        triage_preview: seed('triage_preview', 'triage'),
      },
    });
    // Reset the cached health result since the target just changed.
    setCloudHealth(null);
    setCloudHealthError(null);
  };

  async function handleCheckCloud() {
    if (!config?.endpoints.cloud.base_url || !config?.endpoints.cloud.api_key) {
      setCloudHealthError("Add an API key first.");
      return;
    }
    setCheckingCloud(true);
    setCloudHealth(null);
    setCloudHealthError(null);
    try {
      const s = await invoke("check_llm_health", {
        baseUrl: config.endpoints.cloud.base_url,
        apiKey: config.endpoints.cloud.api_key,
      });
      setCloudHealth(s);
    } catch (e: any) {
      setCloudHealthError(String(e));
    }
    setCheckingCloud(false);
  }

  const cloudConfigured = Boolean(config?.endpoints.cloud.base_url && config?.endpoints.cloud.api_key);

  return (
    <div className="h-screen w-full overflow-y-auto bg-surface">
      <div className="max-w-5xl mx-auto px-10 pt-10 pb-16">
        <header className="mb-8 flex items-start justify-between gap-4">
          <div className="min-w-0">
            <h1 className="text-xl font-semibold text-text mb-1">Settings</h1>
            <p className="text-xs font-mono text-text-muted truncate" title={workspace}>
              {workspace}
            </p>
          </div>
          {onChangeWorkspace && (
            <button
              onClick={onChangeWorkspace}
              className="shrink-0 text-xs border border-stone-2 rounded px-3 py-1 text-text-muted hover:bg-stone-1 transition-colors"
            >
              Change workspace
            </button>
          )}
        </header>

        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div className="flex flex-col gap-4">
            {/* Ingest */}
            <section className="border border-stone-1 rounded-lg p-5 bg-surface-elev flex flex-col gap-3">
              <h2 className="text-sm font-medium text-text">Ingest</h2>
              <p className="text-xs text-text-muted leading-relaxed">
                Import a ChatGPT <code className="font-mono bg-stone-1 px-1 rounded text-[11px]">conversations.json</code>.
              </p>
              <button
                onClick={handleIngest}
                disabled={ingesting}
                className="bg-accent hover:opacity-90 disabled:opacity-50 text-surface text-sm px-4 py-2 rounded font-medium transition-opacity"
              >
                {ingesting ? "Ingesting…" : "Import conversations.json"}
              </button>
              {ingestResult && <p className="text-flag-green text-xs">{ingestResult}</p>}
            </section>

            {/* Local LLM */}
            <section className="border border-stone-1 rounded-lg p-5 bg-surface-elev flex flex-col gap-3">
              <h2 className="text-sm font-medium text-text">Local LLM Endpoint</h2>
              <p className="text-xs text-text-muted leading-relaxed font-mono">
                http://localhost:11434/v1
              </p>
              <button
                onClick={handleCheckLlm}
                disabled={checkingLlm}
                className="bg-stone-1 hover:bg-stone-2 disabled:opacity-50 text-text text-sm px-4 py-2 rounded font-medium transition-colors border border-stone-2"
              >
                {checkingLlm ? "Checking…" : "Check health"}
              </button>

              {healthStatus && (
                <div className="flex flex-col gap-2 mt-2">
                  <ul className="text-xs space-y-1">
                    <HealthRow label="Endpoint" ok={healthStatus.models} okText="Online" badText="Offline" />
                    <HealthRow label="Chat" ok={healthStatus.chat} okText={healthStatus.json_mode ? "OK (JSON mode)" : "OK (no JSON mode)"} badText="Failed" />
                    <HealthRow label="Embeddings" ok={healthStatus.embeddings} okText="OK" badText="Failed" />
                  </ul>
                  {(healthStatus.models_error || healthStatus.chat_error || healthStatus.embeddings_error) && (
                    <div className="border border-flag-red/40 rounded p-2 bg-surface flex flex-col gap-1.5">
                      <div className="text-[10px] font-medium text-flag-red uppercase tracking-wider">What went wrong</div>
                      {healthStatus.models_error && (
                        <ErrorLine label="Endpoint" detail={healthStatus.models_error} hint="Is Ollama running? Try `ollama serve` in a terminal." />
                      )}
                      {healthStatus.chat_error && (
                        <ErrorLine label="Chat" detail={healthStatus.chat_error} hint="Pull a chat model with `ollama pull qwen2.5:7b`." />
                      )}
                      {healthStatus.embeddings_error && (
                        <ErrorLine label="Embeddings" detail={healthStatus.embeddings_error} hint="Pull the embedding model with `ollama pull nomic-embed-text`." />
                      )}
                    </div>
                  )}
                </div>
              )}
            </section>

            {/* Cloud Endpoint */}
            {config && (
              <section className="border border-stone-1 rounded-lg p-5 bg-surface-elev flex flex-col gap-3">
                <h2 className="text-sm font-medium text-text mt-1">Cloud LLM (Optional)</h2>
                <p className="text-xs text-text-muted leading-relaxed">
                  Pick a provider and paste your API key. Cairn will use it for any task you set to <span className="font-mono text-text">Cloud</span>.
                </p>

                <div className="flex flex-col gap-1">
                  <label className="text-[10px] font-medium text-text-muted uppercase tracking-wider">Provider</label>
                  <select
                    value={currentProvider}
                    onChange={(e) => handleSelectProvider(e.target.value as CloudProvider)}
                    className="bg-surface border border-stone-2 rounded px-2 py-1.5 text-sm text-text focus:border-accent outline-none"
                  >
                    {(Object.keys(CLOUD_PROVIDERS) as CloudProvider[]).map((key) => (
                      <option key={key} value={key}>{CLOUD_PROVIDERS[key].label}</option>
                    ))}
                  </select>
                </div>

                <div className="flex flex-col gap-1">
                  <label className="text-[10px] font-medium text-text-muted uppercase tracking-wider">API Key</label>
                  <input
                    type="password"
                    value={config.endpoints.cloud.api_key}
                    onChange={(e) => handleUpdateCloudEndpoint("api_key", e.target.value)}
                    className="bg-surface border border-stone-2 rounded px-2 py-1.5 text-sm text-text focus:border-accent outline-none font-mono"
                    placeholder={currentProvider === 'openai' ? 'sk-...' : currentProvider === 'gemini' ? 'AIza...' : 'API key'}
                  />
                </div>

                <div className="flex items-center gap-2">
                  <button
                    onClick={handleCheckCloud}
                    disabled={checkingCloud || !cloudConfigured}
                    className="bg-stone-1 hover:bg-stone-2 disabled:opacity-50 text-text text-sm px-4 py-2 rounded font-medium transition-colors border border-stone-2"
                  >
                    {checkingCloud ? "Checking…" : "Check health"}
                  </button>
                  {!cloudConfigured && (
                    <span className="text-[11px] text-text-muted">Add an API key to enable.</span>
                  )}
                </div>

                {cloudHealth && (
                  <div className="flex flex-col gap-2 mt-1">
                    <ul className="text-xs space-y-1">
                      <HealthRow label="Endpoint" ok={cloudHealth.models} okText="Online" badText="Offline" />
                      <HealthRow label="Chat" ok={cloudHealth.chat} okText={cloudHealth.json_mode ? "OK (JSON mode)" : "OK (no JSON mode)"} badText="Failed" />
                      <HealthRow label="Embeddings" ok={cloudHealth.embeddings} okText="OK" badText="Failed" />
                    </ul>
                    {(cloudHealth.models_error || cloudHealth.chat_error || cloudHealth.embeddings_error) && (
                      <div className="border border-flag-red/40 rounded p-2 bg-surface flex flex-col gap-1.5">
                        <div className="text-[10px] font-medium text-flag-red uppercase tracking-wider">What went wrong</div>
                        {cloudHealth.models_error && (
                          <ErrorLine label="Endpoint" detail={cloudHealth.models_error} hint={explainCloudError(cloudHealth.models_error, currentProvider)} />
                        )}
                        {cloudHealth.chat_error && (
                          <ErrorLine label="Chat" detail={cloudHealth.chat_error} hint={explainCloudError(cloudHealth.chat_error, currentProvider)} />
                        )}
                        {cloudHealth.embeddings_error && currentProvider === 'gemini' ? (
                          <ErrorLine
                            label="Embeddings"
                            detail={cloudHealth.embeddings_error}
                            hint="Gemini's OpenAI-compat endpoint doesn't serve the embedding model Cairn looks for by default. Embeddings will still run locally — this is expected."
                          />
                        ) : cloudHealth.embeddings_error ? (
                          <ErrorLine label="Embeddings" detail={cloudHealth.embeddings_error} hint={explainCloudError(cloudHealth.embeddings_error, currentProvider)} />
                        ) : null}
                      </div>
                    )}
                  </div>
                )}
                {cloudHealthError && (
                  <div className="border border-flag-red/40 rounded p-2 bg-surface flex flex-col gap-1">
                    <div className="text-[10px] font-medium text-flag-red uppercase tracking-wider">Health check failed</div>
                    <p className="text-[11px] text-text font-mono break-words whitespace-pre-wrap">{cloudHealthError}</p>
                    <p className="text-[11px] text-text-muted">{explainCloudError(cloudHealthError, currentProvider)}</p>
                  </div>
                )}

                <button
                  onClick={() => setShowAdvancedCloud((s) => !s)}
                  className="text-[11px] text-text-muted hover:text-text self-start mt-1"
                >
                  {showAdvancedCloud ? '▾ Advanced' : '▸ Advanced'}
                </button>
                {showAdvancedCloud && (
                  <div className="flex flex-col gap-1 border-t border-stone-1 pt-2">
                    <label className="text-[10px] font-medium text-text-muted uppercase tracking-wider">Base URL</label>
                    <input
                      type="text"
                      value={config.endpoints.cloud.base_url}
                      onChange={(e) => handleUpdateCloudEndpoint("base_url", e.target.value)}
                      disabled={currentProvider !== 'custom'}
                      className="bg-surface border border-stone-2 rounded px-2 py-1.5 text-sm text-text focus:border-accent outline-none font-mono disabled:opacity-60 disabled:cursor-not-allowed"
                      placeholder="https://..."
                    />
                    <p className="text-[10px] text-text-muted leading-relaxed mt-1">
                      {currentProvider === 'custom'
                        ? 'Point to any OpenAI-compatible /v1 base URL.'
                        : 'Base URL is managed by the provider preset. Switch to Custom to override.'}
                    </p>
                  </div>
                )}
              </section>
            )}
          </div>

          <div className="flex flex-col gap-4">
            {/* Pipelines */}
            <section className="border border-stone-1 rounded-lg p-5 bg-surface-elev flex flex-col gap-4">
              <h2 className="text-sm font-medium text-text">LLM Routing</h2>
              <p className="text-xs text-text-muted leading-relaxed">
                Connect the pipelines to your mapped Endpoint targets.
              </p>

              {config && (
                <div className="flex flex-col gap-3 border-b border-stone-1 pb-4 mb-2">
                  {/* Triage Job */}
                  <JobSelect
                    label="Triage model (chunk summarization)"
                    job="triage_preview"
                    config={config}
                    onUpdate={handleUpdateJobModel}
                    onUpdateBackup={handleUpdateJobBackup}
                    localModels={localModels}
                    cloudModels={cloudModels}
                    cloudConfigured={cloudConfigured}
                  />

                  {/* Extraction Job */}
                  <JobSelect
                    label="Reduce model (JSON extraction)"
                    job="extraction"
                    config={config}
                    onUpdate={handleUpdateJobModel}
                    onUpdateBackup={handleUpdateJobBackup}
                    localModels={localModels}
                    cloudModels={cloudModels}
                    cloudConfigured={cloudConfigured}
                  />

                  {/* Chat Job */}
                  <JobSelect
                    label="Chat model (Generative responses)"
                    job="chat"
                    config={config}
                    onUpdate={handleUpdateJobModel}
                    onUpdateBackup={handleUpdateJobBackup}
                    localModels={localModels}
                    cloudModels={cloudModels}
                    cloudConfigured={cloudConfigured}
                  />
                </div>
              )}

              {/* Extraction Execution Status */}
              <h3 className="text-xs font-semibold text-text uppercase opacity-80 mt-1">Extraction Sync</h3>
              {status && (
                <div className="grid grid-cols-3 gap-px bg-stone-1 rounded overflow-hidden mt-1">
                  <Cell label="Total" value={status.conversations} />
                  <Cell label="Done" value={status.done} />
                  <Cell label="In flight" value={status.processing} />
                  <Cell label="Pending" value={status.pending} />
                  <Cell label="Failed" value={status.failed} tone={status.failed > 0 ? 'warn' : undefined} />
                  <Cell label="Parked" value={status.parked} />
                </div>
              )}

              {extractionRunning && extractionProgress && (
                <div className="text-xs text-text-muted flex items-center gap-2 flex-wrap">
                  <span className="font-mono">{extractionProgress.current} / {extractionProgress.total}</span>
                  {status?.processing ? <span className="text-accent">· {status.processing} in flight</span> : null}
                  {extractionProgress.failed > 0 && <span className="text-flag-amber">· {extractionProgress.failed} failed</span>}
                  {syncStartedAt && (
                    <span className="font-mono text-text-muted">
                      · {formatElapsed(Date.now() - syncStartedAt)}
                      {extractionProgress.current > 0 && ` · ${formatRate(extractionProgress.current, Date.now() - syncStartedAt)}/min`}
                    </span>
                  )}
                </div>
              )}
              <div className="flex gap-2 mt-1">
                <button
                  onClick={onSyncAll}
                  disabled={extractionRunning || !onSyncAll}
                  className="flex-1 bg-stone-1 hover:bg-stone-2 disabled:opacity-50 text-text text-sm px-4 py-2 rounded font-medium transition-colors border border-stone-2"
                >
                  {extractionStopping ? "Stopping sync" : extractionRunning ? "Syncing…" : "Sync all"}
                </button>
                {extractionRunning && onStop && (
                  <button
                    onClick={onStop}
                    disabled={extractionStopping}
                    className="bg-surface hover:bg-stone-1 disabled:opacity-50 text-text text-sm px-4 py-2 rounded font-medium transition-colors border border-stone-2"
                  >
                    {extractionStopping ? "Start Again" : "Stop"}
                  </button>
                )}
              </div>
            </section>
          </div>
        </div>

        {failures.length > 0 && (
          <section className="mt-6 border border-flag-amber/40 rounded-lg p-5 bg-surface-elev">
            <div className="flex items-center justify-between mb-3">
              <div>
                <h2 className="text-sm font-medium text-text">
                  Failed extractions <span className="text-flag-amber font-mono">({failures.length})</span>
                </h2>
                <p className="text-xs text-text-muted leading-relaxed mt-1">
                  Conversations whose extraction never produced valid JSON.
                </p>
              </div>
              <div className="flex items-center gap-2 shrink-0">
                <button
                  onClick={handleRetryAllFailed}
                  disabled={retryingAllFailed || failures.length === 0}
                  className="px-3 py-1 rounded border border-stone-2 text-text text-xs hover:bg-stone-1 transition-colors disabled:opacity-50"
                >
                  {retryingAllFailed ? 'Queueing…' : 'Retry all failed'}
                </button>
                <button onClick={() => setShowFailures((s) => !s)} className="text-xs text-text-muted hover:text-text shrink-0">
                  {showFailures ? 'Hide' : 'Show'}
                </button>
              </div>
            </div>
            {showFailures && (
              <ul className="divide-y divide-stone-1 border border-stone-1 rounded overflow-hidden">
                {failures.map((f) => (
                  <li key={f.id} className="flex items-start gap-3 p-3 text-xs">
                    <div className="min-w-0 flex-1">
                      <div className="text-text text-sm truncate" title={f.title}>{f.title}</div>
                      <div
                        className="text-text-muted font-mono mt-1 break-words overflow-hidden"
                        title={f.error}
                        style={{
                          display: '-webkit-box',
                          WebkitBoxOrient: 'vertical',
                          WebkitLineClamp: 2,
                        }}
                      >
                        {f.error || '(no error recorded)'}
                      </div>
                    </div>
                    <div className="flex flex-col gap-1 shrink-0">
                      <button onClick={() => handleRetry(f.id)} className="px-2 py-1 rounded border border-stone-2 text-text hover:bg-stone-1 transition-colors">Retry</button>
                      <button onClick={() => handleSkip(f.id)} className="px-2 py-1 rounded border border-stone-2 text-text-muted hover:bg-stone-1 transition-colors">Skip</button>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </section>
        )}

        <section className="mt-8 border border-flag-red/40 rounded-lg p-5 bg-surface-elev">
          <h2 className="text-sm font-medium text-flag-red mb-1">Danger zone</h2>
          <button onClick={handleReset} className="bg-surface hover:bg-flag-red hover:text-surface text-flag-red text-sm px-4 py-2 rounded font-medium transition-colors border border-flag-red/60">
            Reset workspace
          </button>
        </section>
      </div>
    </div>
  );
}

function JobSelect({
  label,
  job,
  config,
  onUpdate,
  onUpdateBackup,
  localModels,
  cloudModels,
  cloudConfigured,
}: {
  label: string;
  job: keyof WorkspaceConfig['jobs'];
  config: WorkspaceConfig;
  onUpdate: (job: keyof WorkspaceConfig['jobs'], endpoint: string, model: string) => void;
  onUpdateBackup: (job: keyof WorkspaceConfig['jobs'], backup_endpoint: string, backup_model: string) => void;
  localModels: string[];
  cloudModels: string[];
  cloudConfigured: boolean;
}) {
  const j = config.jobs[job];
  const currentEp = j.endpoint;
  const effectiveEp = currentEp === 'cloud' && !cloudConfigured ? 'default' : currentEp;
  const models = effectiveEp === 'cloud' ? cloudModels : localModels;
  const backupEp = (j.backup_endpoint && j.backup_endpoint !== '') ? j.backup_endpoint : 'none';
  const backupModels = backupEp === 'cloud' ? cloudModels : backupEp === 'default' ? localModels : [];
  const pickModelForEndpoint = (endpoint: string) => {
    if (endpoint === 'cloud' && inferCloudProvider(config.endpoints.cloud) === 'custom') {
      return cloudModels[0] || defaultModelForJob(job, endpoint, config);
    }
    return defaultModelForJob(job, endpoint, config);
  };

  return (
    <div className="flex flex-col gap-1.5">
      <label className="text-[10px] font-medium text-text-muted uppercase tracking-wider">{label}</label>
      <div className="flex gap-2 items-center">
        <span className="text-[10px] font-medium text-text-muted uppercase tracking-wider w-14 shrink-0">Pref.</span>
        <select
          value={currentEp}
          onChange={(e) => onUpdate(job, e.target.value, pickModelForEndpoint(e.target.value))}
          className="bg-surface border border-stone-2 rounded px-2 py-1 text-xs text-text focus:border-accent outline-none w-24"
        >
          <option value="default">Local</option>
          <option value="cloud" disabled={!cloudConfigured}>
            {cloudConfigured ? 'Cloud' : 'Cloud (not set up)'}
          </option>
        </select>
        <select
          value={j.model}
          onChange={(e) => onUpdate(job, currentEp, e.target.value)}
          className="bg-surface border border-stone-2 rounded px-2 py-1 text-xs text-text focus:border-accent outline-none flex-1"
        >
          {models.length === 0 && j.model && (
            <option value={j.model}>{j.model}</option>
          )}
          {models.map(m => <option key={m} value={m}>{m}</option>)}
        </select>
      </div>

      <div className="flex gap-2 items-center">
        <span className="text-[10px] font-medium text-text-muted uppercase tracking-wider w-14 shrink-0">Backup</span>
        <select
          value={backupEp}
          onChange={(e) => {
            const ep = e.target.value;
            if (ep === 'none') {
              onUpdateBackup(job, '', '');
            } else {
              onUpdateBackup(job, ep, pickModelForEndpoint(ep));
            }
          }}
          className="bg-surface border border-stone-2 rounded px-2 py-1 text-xs text-text focus:border-accent outline-none w-24"
        >
          <option value="none">None</option>
          <option value="default">Local</option>
          <option value="cloud" disabled={!cloudConfigured}>
            {cloudConfigured ? 'Cloud' : 'Cloud (not set up)'}
          </option>
        </select>
        <select
          value={j.backup_model || ''}
          disabled={backupEp === 'none'}
          onChange={(e) => onUpdateBackup(job, backupEp, e.target.value)}
          className="bg-surface border border-stone-2 rounded px-2 py-1 text-xs text-text focus:border-accent outline-none flex-1 disabled:opacity-50"
        >
          {backupEp === 'none' && <option value="">—</option>}
          {backupModels.length === 0 && j.backup_model && (
            <option value={j.backup_model}>{j.backup_model}</option>
          )}
          {backupModels.map(m => <option key={m} value={m}>{m}</option>)}
        </select>
      </div>

      {currentEp === 'cloud' && !cloudConfigured && (
        <p className="text-[10px] text-flag-amber">
          Preferred is Cloud but it's not set up — this job will run on the local LLM until you add an API key above.
        </p>
      )}
    </div>
  );
}

function HealthRow({ label, ok, okText, badText }: { label: string; ok: boolean; okText: string; badText: string; }) {
  return (
    <li className="flex justify-between">
      <span className="text-text-muted">{label}</span>
      <span className={ok ? "text-flag-green font-medium" : "text-flag-red font-medium"}>{ok ? okText : badText}</span>
    </li>
  );
}

function ErrorLine({ label, detail, hint }: { label: string; detail: string; hint?: string }) {
  const [expanded, setExpanded] = useState(false);
  return (
    <div className="flex flex-col gap-0.5">
      <div className="flex items-center gap-2">
        <span className="text-[10px] font-medium text-text-muted uppercase tracking-wider shrink-0">{label}</span>
        <button
          onClick={() => setExpanded((s) => !s)}
          className="text-[10px] text-text-muted hover:text-text"
        >
          {expanded ? 'Hide detail' : 'Show detail'}
        </button>
        <button
          onClick={() => navigator.clipboard?.writeText(detail)}
          className="text-[10px] text-text-muted hover:text-text"
        >
          Copy
        </button>
      </div>
      {hint && <p className="text-[11px] text-text leading-relaxed">{hint}</p>}
      {expanded && (
        <pre className="text-[10px] font-mono text-text-muted whitespace-pre-wrap break-words bg-stone-1 rounded p-2 max-h-40 overflow-auto">
          {detail}
        </pre>
      )}
    </div>
  );
}

// Translate raw error text into an actionable hint. Looks for well-known HTTP
// codes and provider-specific quirks so users aren't left staring at a stack trace.
function explainCloudError(err: string, provider: string): string {
  const e = err.toLowerCase();
  if (e.includes('http 401') || e.includes('unauthorized') || e.includes('invalid api key') || e.includes('api_key_invalid')) {
    return provider === 'gemini'
      ? 'Gemini rejected the API key. Generate one at https://aistudio.google.com/apikey and paste it above.'
      : provider === 'openai'
        ? 'OpenAI rejected the API key. Check it at https://platform.openai.com/api-keys.'
        : 'The endpoint rejected the API key. Verify the key matches this provider.';
  }
  if (e.includes('http 403') || e.includes('permission_denied') || e.includes('forbidden')) {
    return 'The key is valid but doesn\'t have access to this endpoint or model. Check the key\'s project permissions and enabled APIs.';
  }
  if (e.includes('http 404') || e.includes('not found')) {
    if (e.includes('models/') || e.includes('model')) {
      return 'The provider doesn\'t recognize this model name. Pick one that exists on that endpoint in LLM Routing.';
    }
    return provider === 'gemini'
      ? 'Gemini returned 404 — usually a Base URL path issue. Check Advanced → Base URL.'
      : 'Endpoint returned 404. The Base URL may be wrong; check Advanced.';
  }
  if (e.includes('http 429') || e.includes('rate limit') || e.includes('quota')) {
    return 'Rate-limited or quota exhausted. Wait a minute and try again, or upgrade the provider plan.';
  }
  if (e.includes('http 400') && (e.includes('model') || e.includes('not supported'))) {
    return 'The model name isn\'t recognized by the provider. Pick a different model in LLM Routing.';
  }
  if (e.includes('timeout') || e.includes('timed out')) {
    return 'The provider didn\'t respond within 30s. Retry, or check your network.';
  }
  if (e.includes('network error') || e.includes('dns') || e.includes('connect')) {
    return 'Couldn\'t reach the endpoint. Check your network / firewall and the Base URL (Advanced).';
  }
  if (e.includes('non-json') || e.includes('unexpected models response')) {
    return 'The endpoint returned HTML or a malformed body — usually a wrong Base URL. Check Advanced.';
  }
  return 'Expand "Show detail" above for the raw error, then copy it if you need help.';
}

function Cell({ label, value, tone }: { label: string; value: number; tone?: 'warn'; }) {
  return (
    <div className="bg-surface-elev px-3 py-2">
      <div className="text-[10px] font-medium text-text-muted uppercase tracking-wider">{label}</div>
      <div className={`text-base font-semibold tabular-nums mt-0.5 ${tone === 'warn' ? 'text-flag-amber' : 'text-text'}`}>
        {value.toLocaleString()}
      </div>
    </div>
  );
}

function formatElapsed(ms: number): string {
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return `${m}m ${s % 60}s`;
}

function formatRate(completed: number, elapsedMs: number): string {
  if (elapsedMs <= 0) return '0';
  return (completed / (elapsedMs / 60000)).toFixed(1);
}
