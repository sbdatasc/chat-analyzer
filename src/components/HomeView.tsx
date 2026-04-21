import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

type Tile = { id: string; name: string; kind: string };

type HomeData = {
  recent: Tile[];
  orphan_count: number;
  orphan_since: string;
  snapshots: Tile[];
  growing: Tile[];
};

type Status = {
  conversations: number;
  messages: number;
  nodes: number;
  concepts: number;
  topics: number;
  entities: number;
  pending: number;
  processing: number;
  done: number;
  failed: number;
  parked: number;
};

export function HomeView({
  workspace,
  onAskPrompt,
  onLens,
  onLoadSnapshot,
  extractionRunning = false,
}: {
  workspace: string;
  onAskPrompt: () => void;
  onLens: (lens: string) => void;
  onLoadSnapshot: (id: string) => void;
  extractionRunning?: boolean;
}) {
  const [data, setData] = useState<HomeData | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [tiles, st] = await Promise.all([
        invoke<HomeData>('get_home_tiles', { workspacePath: workspace }),
        invoke<Status>('get_workspace_status', { workspacePath: workspace }),
      ]);
      setData(tiles);
      setStatus(st);
    } catch (e) {
      setError(String(e));
    }
  }, [workspace]);

  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, extractionRunning ? 2000 : 10000);
    return () => clearInterval(interval);
  }, [refresh, extractionRunning]);

  if (error) {
    return (
      <div className="h-full w-full flex items-center justify-center px-6">
        <p className="text-flag-red text-sm font-mono">{error}</p>
      </div>
    );
  }

  if (!status || !data) {
    return <div className="h-full w-full bg-surface" />;
  }

  const hasArchive = status.conversations > 0;
  const hasExtractions = status.done > 0;
  const extractionInProgress = extractionRunning || status.processing > 0;
  const remaining = status.pending + status.failed;

  return (
    <div className="h-full w-full overflow-y-auto bg-surface">
      <div className="max-w-xl mx-auto px-8 py-10 flex flex-col gap-8">
        <header>
          <h1 className="text-lg font-semibold text-text mb-1">Cairn</h1>
          <p className="text-sm text-text-muted">
            A trail through your own thinking.
          </p>
        </header>

        {/* Status strip */}
        <section className="grid grid-cols-4 gap-px bg-stone-1 rounded overflow-hidden">
          <Stat label="Conversations" value={status.conversations} />
          <Stat label="Extracted" value={status.done} />
          <Stat label="Pending" value={remaining} />
          <Stat label="Parked" value={status.parked} />
        </section>

        {/* Extraction progress — purely informational now. Auto-started after
            ingest; manual Sync all lives in Settings. */}
        {hasArchive && (extractionInProgress || (!hasExtractions && remaining > 0)) && (
          <section className="border border-stone-1 rounded-lg p-5 bg-surface-elev">
            <h2 className="text-sm font-medium text-text mb-1">
              {extractionInProgress ? 'Extracting' : 'Extraction pending'}
            </h2>
            <p className="text-sm text-text-muted">
              {status.done} done, {remaining} remaining.
              {!extractionInProgress && ' Open Settings to Sync all.'}
            </p>
          </section>
        )}

        {/* Only render tiles that have data to show. Hide empty shells. */}
        {hasExtractions && (
          <section className="grid grid-cols-2 gap-4">
            {data.recent.length > 0 && (
              <Tile heading="Recently added">
                <ul className="space-y-1">
                  {data.recent.map((t) => (
                    <li
                      key={t.id}
                      className="text-sm text-text truncate"
                      title={t.name}
                    >
                      {t.name}
                    </li>
                  ))}
                </ul>
              </Tile>
            )}

            {data.growing.length > 0 && (
              <Tile heading="Growing">
                <ul className="space-y-1">
                  {data.growing.map((t) => (
                    <li
                      key={t.id}
                      className="text-sm text-text truncate"
                      title={t.name}
                    >
                      {t.name}
                    </li>
                  ))}
                </ul>
              </Tile>
            )}

            {data.orphan_count > 0 && (
              <button
                onClick={() => onLens('lens_orphans')}
                className="border border-stone-1 rounded-lg p-4 bg-surface-elev text-left hover:border-accent transition-colors"
              >
                <h3 className="text-[10px] font-medium text-text-muted mb-2 uppercase tracking-wider">
                  Orphans
                </h3>
                <p className="text-sm text-text">
                  <span className="font-mono">{data.orphan_count}</span> concept
                  {data.orphan_count === 1 ? '' : 's'} untouched since{' '}
                  {data.orphan_since}.
                </p>
              </button>
            )}

            <button
              onClick={onAskPrompt}
              className="border border-stone-1 rounded-lg p-4 bg-surface-elev text-left hover:border-accent transition-colors"
            >
              <h3 className="text-[10px] font-medium text-text-muted mb-2 uppercase tracking-wider">
                Ask the graph
              </h3>
              <p className="text-sm text-text">
                Open the chat panel and ask a question.
              </p>
            </button>
          </section>
        )}

        {data.snapshots.length > 0 && (
          <section>
            <h3 className="text-[10px] font-medium text-text-muted mb-2 uppercase tracking-wider">
              Snapshots
            </h3>
            <ul className="space-y-1">
              {data.snapshots.map((s) => (
                <li key={s.id}>
                  <button
                    onClick={() => onLoadSnapshot(s.id)}
                    className="text-sm text-text hover:text-accent transition-colors"
                  >
                    {s.name}
                  </button>
                </li>
              ))}
            </ul>
          </section>
        )}

        {!hasArchive && (
          <section>
            <p className="text-sm text-text-muted">
              No archive yet. Drop a conversations.json in the window to begin.
            </p>
          </section>
        )}
      </div>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: number }) {
  return (
    <div className="bg-surface-elev px-4 py-3">
      <div className="text-[10px] font-medium text-text-muted uppercase tracking-wider">
        {label}
      </div>
      <div className="text-xl font-semibold text-text tabular-nums mt-1">
        {value.toLocaleString()}
      </div>
    </div>
  );
}

function Tile({
  heading,
  children,
}: {
  heading: string;
  children: React.ReactNode;
}) {
  return (
    <section className="border border-stone-1 rounded-lg p-4 bg-surface-elev">
      <h3 className="text-[10px] font-medium text-text-muted mb-2 uppercase tracking-wider">
        {heading}
      </h3>
      {children}
    </section>
  );
}
