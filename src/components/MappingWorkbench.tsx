import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

export function MappingWorkbench({
  conversationId,
  workspace,
  onClose
}: {
  conversationId: string;
  workspace: string;
  onClose: () => void;
}) {
  const [data, setData] = useState<any>(null);

  useEffect(() => {
    loadData();
  }, [conversationId]);

  const loadData = async () => {
    try {
      const res = await invoke("get_conversation_workbench", {
        conversationId,
        workspacePath: workspace
      });
      setData(res);
    } catch (e) {
      console.error("Workbench failed to load:", e);
    }
  };

  const handleApprove = async (id: number) => {
    try {
      await invoke("approve_parked_extraction", { extractionId: id, workspacePath: workspace });
      await loadData();
    } catch (e) {
      console.error("Approval failed:", e);
    }
  };

  const handleReject = async (id: number) => {
    try {
      await invoke("reject_parked_extraction", { extractionId: id, workspacePath: workspace });
      await loadData();
    } catch (e) {
      console.error("Rejection failed:", e);
    }
  };

  if (!data) {
    return (
      <div className="absolute inset-0 bg-surface z-50 flex items-center justify-center text-text opacity-95">
        Loading...
      </div>
    );
  }

  return (
    <div className="absolute inset-0 bg-surface z-50 flex flex-row border-t border-stone-2 shadow-2xl animate-in slide-in-from-bottom-5">
      {/* Messages Column (Left) */}
      <div className="flex-[3] flex flex-col border-r border-stone-1 overflow-hidden relative">
        <div className="p-4 border-b border-stone-1 bg-surface-elev flex justify-between items-center z-10 shadow-sm">
          <h2 className="font-semibold text-lg">Source Document</h2>
          <span className="text-xs text-text-muted select-all bg-stone-1 px-2 py-1 rounded">{conversationId}</span>
        </div>
        <div className="flex-1 overflow-y-auto p-6 flex flex-col gap-4">
          {data.messages.map((msg: any) => (
            <div key={msg.message_id} className={`flex flex-col ${msg.role === 'user' ? 'items-end' : 'items-start'}`}>
              <div className={`px-4 py-2 rounded-lg max-w-[85%] text-sm leading-relaxed ${
                msg.role === 'user' ? 'bg-stone-1 text-text' : 'text-text'
              }`}>
                {msg.text}
              </div>
            </div>
          ))}
        </div>
      </div>

      {/* Extractions Column (Right) */}
      <div className="flex-[2] flex flex-col bg-surface-elev overflow-hidden">
        <div className="p-4 border-b border-stone-1 flex justify-between items-center shadow-sm">
          <h2 className="font-semibold text-lg">Extracted Concepts</h2>
          <button onClick={onClose} className="text-sm px-3 py-1 bg-stone-2 hover:bg-stone-1 rounded transition-colors text-text border border-transparent hover:border-stone-2">
            Close
          </button>
        </div>
        <div className="flex-1 overflow-y-auto p-6 flex flex-col gap-6">
          
          {/* Pending Review Block */}
          {data.parked_items.length > 0 && (
            <div className="flex flex-col gap-3">
              <h3 className="text-sm font-semibold tracking-wider text-accent uppercase mb-2">Pending Review (&lt;0.60)</h3>
              {data.parked_items.map((item: any) => {
                let payloadObj: any = {};
                try {
                  payloadObj = JSON.parse(item.payload);
                } catch { 
                  payloadObj = { name: "Unknown" };
                }

                return (
                  <div key={item.id} className="border border-stone-2 bg-surface p-3 rounded flex flex-col gap-2 shadow-sm">
                    <div className="flex justify-between items-start">
                      <span className="font-medium text-sm text-text">{payloadObj.name}</span>
                      <span className="text-[10px] bg-stone-1 text-text-muted px-2 py-0.5 rounded uppercase">{item.kind}</span>
                    </div>
                    <p className="text-xs text-text-muted italic">Confidence: {item.confidence}</p>
                    {payloadObj.description && <p className="text-xs text-text mt-1">{payloadObj.description}</p>}
                    
                    <div className="flex flex-row justify-end gap-2 mt-2 pt-2 border-t border-stone-1">
                      <button onClick={() => handleReject(item.id)} className="text-xs text-text-muted hover:text-red-400 px-3 py-1 rounded hover:bg-stone-1 transition-colors">Reject</button>
                      <button onClick={() => handleApprove(item.id)} className="text-xs bg-accent text-surface px-4 py-1 rounded hover:opacity-90 transition-opacity font-medium shadow-sm">Approve</button>
                    </div>
                  </div>
                );
              })}
            </div>
          )}

          {/* Solid Entities Block */}
          <div className="flex flex-col gap-3">
            <h3 className="text-sm font-semibold tracking-wider text-text-muted uppercase mb-2">Mapped to Graph</h3>
            {data.solid_nodes.length === 0 && <p className="text-sm text-text-muted italic">None yet.</p>}
            <div className="flex flex-row flex-wrap gap-2">
              {data.solid_nodes.map((node: any) => (
                <div key={node.id} className="border border-stone-1 bg-surface px-2 py-1 flex flex-row items-center gap-2 rounded shadow-sm hover:border-stone-2 transition-colors cursor-default">
                  <div className={`w-2 h-2 rounded-full ${node.node_type === 'entity' ? 'bg-accent' : 'bg-stone-2'}`} />
                  <span className="text-xs text-text font-medium">{node.name}</span>
                </div>
              ))}
            </div>
          </div>

        </div>
      </div>
    </div>
  );
}
