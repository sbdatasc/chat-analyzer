import { useState, useRef, useEffect } from "react";

export interface ChatMessage {
  id: string;
  role: 'user' | 'assistant';
  text: string;
}

function renderMessageText(text: string) {
  // Split on citation markers like [^1] [^12] and render each as an inline pill.
  const parts = text.split(/(\[\^\d+\])/g);
  return parts.map((part, index) => {
    const match = part.match(/\[\^(\d+)\]/);
    if (match) {
      return (
        <sup
          key={index}
          className="inline-flex items-center justify-center bg-accent-soft text-text text-[10px] min-w-[14px] h-[14px] px-1 rounded-sm mx-0.5 font-mono align-super cursor-default"
          title={`Source ${match[1]}`}
        >
          {match[1]}
        </sup>
      );
    }
    return <span key={index}>{part}</span>;
  });
}

export function ChatPanel({
  onSearch,
  onLens,
  messages,
  loading,
}: {
  onSearch: (q: string) => void;
  onLens: (lens: string) => void;
  messages: ChatMessage[];
  loading: boolean;
}) {
  const [query, setQuery] = useState("");
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, loading]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (query.trim() && !loading) {
      onSearch(query);
      setQuery("");
    }
  };

  return (
    <div className="w-full h-full flex flex-col bg-surface-elev relative">
      <div className="flex-1 overflow-y-auto px-8 py-6">
        {messages.length === 0 ? (
          <div className="h-full flex flex-col items-center justify-center gap-2">
            <p className="text-text-muted text-sm">Ask or click a lens.</p>
            <p className="text-text-muted text-xs font-mono">
              Territory · Drift · Bridges · Orphans · Path
            </p>
          </div>
        ) : (
          <div className="flex flex-col gap-6 max-w-prose mx-auto">
            {messages.map((msg) => (
              <div key={msg.id} className="flex flex-col gap-1">
                <span className="text-[10px] uppercase tracking-wider text-text-muted font-medium">
                  {msg.role === 'user' ? 'You' : 'Cairn'}
                </span>
                <div
                  className={`text-sm leading-relaxed ${
                    msg.role === 'user'
                      ? 'text-text'
                      : 'text-text'
                  } whitespace-pre-wrap`}
                >
                  {renderMessageText(msg.text)}
                </div>
              </div>
            ))}
            {loading && (
              <div className="flex flex-col gap-1 opacity-60">
                <span className="text-[10px] uppercase tracking-wider text-text-muted font-medium">
                  Cairn
                </span>
                <div className="text-sm text-text-muted">Synthesizing…</div>
              </div>
            )}
            <div ref={endRef} />
          </div>
        )}
      </div>

      <div className="px-8 py-4 border-t border-stone-1 bg-surface-elev flex flex-col gap-3">
        <form
          onSubmit={handleSubmit}
          className="flex flex-row items-center border border-stone-2 rounded bg-surface focus-within:border-accent transition-colors"
        >
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            disabled={loading}
            placeholder="Ask the archive"
            className="flex-1 bg-transparent border-none outline-none text-text text-sm px-3 py-2 disabled:opacity-50"
          />
          <button
            type="submit"
            disabled={loading || !query.trim()}
            className="text-accent text-sm font-medium px-3 py-2 disabled:opacity-40"
          >
            Trace
          </button>
        </form>
        <div className="flex flex-row gap-2 overflow-x-auto">
          {(['lens_territory', 'lens_drift', 'lens_bridges', 'lens_orphans', 'lens_path'] as const).map(
            (cmd) => {
              const label = cmd.replace('lens_', '');
              return (
                <button
                  key={cmd}
                  onClick={() => onLens(cmd)}
                  disabled={loading}
                  className="px-3 py-1 rounded bg-surface border border-stone-1 hover:border-accent hover:text-accent text-text text-xs capitalize whitespace-nowrap disabled:opacity-50 transition-colors"
                >
                  {label}
                </button>
              );
            }
          )}
        </div>
      </div>
    </div>
  );
}
