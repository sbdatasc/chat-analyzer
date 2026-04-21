import { useRef, useState, useEffect, useMemo, useCallback } from 'react';
import ForceGraph2D, { ForceGraphMethods } from 'react-force-graph-2d';

type GraphNode = {
  id: string;
  name: string;
  group: 'conversation' | 'entity' | 'concept' | 'topic' | 'pattern';
  x?: number;
  y?: number;
  visited?: boolean; // For highlighting logic
};

type GraphLink = {
  source: string | GraphNode;
  target: string | GraphNode;
  type?: string;
  value?: number;
};

type GraphData = {
  nodes: GraphNode[];
  links: GraphLink[];
};

const FIT_PADDING = 40;
const ZOOM_STEP = 1.25;
const MIN_ZOOM = 0.4;
const MAX_ZOOM = 8;

export function GraphCanvas({
  data,
  onNodeClick,
}: {
  data: GraphData | null | undefined;
  onNodeClick?: (node: GraphNode) => void;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const fgRef = useRef<ForceGraphMethods<GraphNode, GraphLink> | undefined>(undefined);
  const [dimensions, setDimensions] = useState({ width: 800, height: 600 });
  const zoomLevelRef = useRef(1);
  const pendingAutoFitRef = useRef(false);

  const [hoverNode, setHoverNode] = useState<GraphNode | null>(null);
  const [activeNode, setActiveNode] = useState<GraphNode | null>(null);

  // Measure container dimensions
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const observer = new ResizeObserver((entries) => {
      if (entries.length > 0) {
        setDimensions({
          width: entries[0].contentRect.width,
          height: entries[0].contentRect.height,
        });
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // Compute highlighting topology
  const highlightNodes = useMemo(() => {
    const set = new Set<string>();
    if (!activeNode && !hoverNode) return set;
    
    const root = hoverNode || activeNode;
    if (root) {
      set.add(root.id);
      data?.links.forEach((link) => {
        const srcId = typeof link.source === 'object' ? link.source.id : link.source;
        const tgtId = typeof link.target === 'object' ? link.target.id : link.target;
        if (srcId === root.id) set.add(tgtId);
        if (tgtId === root.id) set.add(srcId);
      });
    }
    return set;
  }, [data, hoverNode, activeNode]);

  const highlightLinks = useMemo(() => {
    const set = new Set<GraphLink>();
    if (!activeNode && !hoverNode) return set;
    
    const root = hoverNode || activeNode;
    if (root) {
      data?.links.forEach((link) => {
        const srcId = typeof link.source === 'object' ? link.source.id : link.source;
        const tgtId = typeof link.target === 'object' ? link.target.id : link.target;
        if (srcId === root.id || tgtId === root.id) set.add(link);
      });
    }
    return set;
  }, [data, hoverNode, activeNode]);

  const fitToView = useCallback((durationMs = 350) => {
    if (!fgRef.current || !data || data.nodes.length === 0) return;
    fgRef.current.zoomToFit(durationMs, FIT_PADDING);
    zoomLevelRef.current = fgRef.current.zoom();
  }, [data]);

  useEffect(() => {
    if (!data || data.nodes.length === 0) return;
    pendingAutoFitRef.current = true;
    setActiveNode(null);
    const raf = requestAnimationFrame(() => fitToView(0));
    return () => cancelAnimationFrame(raf);
  }, [data, dimensions.width, dimensions.height, fitToView]);

  const adjustZoom = useCallback((factor: number) => {
    if (!fgRef.current) return;
    const current = fgRef.current.zoom();
    const next = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, current * factor));
    fgRef.current.zoom(next, 250);
    zoomLevelRef.current = next;
  }, []);

  // Handle focus pan/zoom
  const handleNodeClick = useCallback(
    (node: GraphNode) => {
      setActiveNode(node === activeNode ? null : node);
      if (fgRef.current) {
        if (node !== activeNode) {
          fgRef.current.centerAt(node.x, node.y, 1000);
          fgRef.current.zoom(3, 1000);
        }
      }
      if (onNodeClick) onNodeClick(node);
    },
    [activeNode, onNodeClick]
  );

  // Paint node beautifully
  const paintNode = useCallback((node: any, ctx: CanvasRenderingContext2D, globalScale: number) => {
    const isTopic = node.group === 'topic';
    const isConv = node.group === 'conversation';
    const isEntity = node.group === 'entity';

    // Base color mapping
    let fill = '#f5f5f4'; // default stone-1
    let stroke = '#d6d3d1'; // default stone-3
    if (isTopic) fill = '#1A6B32'; // Accent green for topics
    if (isConv) fill = '#e7e5e4'; // stone-2
    if (isEntity) fill = '#1A6B32';

    // Hover or Highlight Logic
    const isHighlighted = highlightNodes.has(node.id);
    const hasFocus = activeNode || hoverNode;
    if (hasFocus && !isHighlighted) {
      // Dim node
      fill = '#fafaf9'; // stone-50
      stroke = '#f5f5f4'; // stone-1
      ctx.globalAlpha = 0.2;
    } else {
      ctx.globalAlpha = 1.0;
      if (isHighlighted && !isTopic) stroke = '#1A6B32';
    }

    const size = isTopic ? 6 : isConv ? 4 : isEntity ? 3 : 2.5;

    ctx.beginPath();
    ctx.arc(node.x, node.y, size, 0, 2 * Math.PI, false);
    ctx.fillStyle = fill;
    ctx.fill();
    ctx.lineWidth = isHighlighted ? 2 / globalScale : 1 / globalScale;
    ctx.strokeStyle = stroke;
    ctx.stroke();

    // Draw Labels if it's a topic, highly connected conversation, or highlighted
    const showLabel = isHighlighted || globalScale > 2 || isTopic;
    if (showLabel && ctx.globalAlpha > 0.5) {
      const label = node.name.length > 25 ? node.name.substring(0, 23) + '…' : node.name;
      ctx.font = `${4 / globalScale}px var(--font-sans)`;
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillStyle = isTopic ? '#ffffff' : '#292524'; // text or surface

      if (isTopic) {
         ctx.fillText(label, node.x, node.y);
      } else {
         const yOfs = size + 3;
         ctx.fillText(label, node.x, node.y + yOfs);
      }
    }
    ctx.globalAlpha = 1.0;
  }, [highlightNodes, activeNode, hoverNode]);


  if (!data || data.nodes.length === 0) {
    return (
      <div className="w-full h-full flex items-center justify-center text-text-muted text-sm">
        No graph data available.
      </div>
    );
  }

  return (
    <div ref={containerRef} className="w-full h-full bg-surface relative overflow-hidden">
      <div className="absolute top-4 right-4 z-20 flex items-center gap-2">
        <button
          onClick={() => adjustZoom(1 / ZOOM_STEP)}
          className="w-9 h-9 rounded border border-stone-2 bg-surface-elev text-text text-lg leading-none hover:bg-stone-1 transition-colors"
          aria-label="Zoom out"
          title="Zoom out"
        >
          −
        </button>
        <button
          onClick={() => fitToView()}
          className="px-3 h-9 rounded border border-stone-2 bg-surface-elev text-text text-xs font-medium hover:bg-stone-1 transition-colors"
          aria-label="Fit graph to view"
          title="Fit graph to view"
        >
          Fit
        </button>
        <button
          onClick={() => adjustZoom(ZOOM_STEP)}
          className="w-9 h-9 rounded border border-stone-2 bg-surface-elev text-text text-lg leading-none hover:bg-stone-1 transition-colors"
          aria-label="Zoom in"
          title="Zoom in"
        >
          +
        </button>
      </div>
      <ForceGraph2D
        ref={fgRef}
        width={dimensions.width}
        height={dimensions.height}
        graphData={data}
        nodeLabel={(n: any) => n.name}
        nodeColor={(n: any) => (n.group === 'topic' ? '#1A6B32' : '#e7e5e4')}
        nodeRelSize={4}
        linkColor={(l: any) => {
          if (activeNode || hoverNode) {
            return highlightLinks.has(l as GraphLink) ? '#1A6B32' : 'rgba(231, 229, 228, 0.2)';
          }
          return '#e7e5e4'; // stone-2
        }}
        linkWidth={(l: any) => (highlightLinks.has(l as GraphLink) ? 2 : 1)}
        linkDirectionalParticles={(l: any) => (highlightLinks.has(l as GraphLink) ? 4 : 0)}
        linkDirectionalParticleWidth={1.5}
        nodeCanvasObject={paintNode}
        onNodeClick={handleNodeClick}
        onNodeHover={(n: any) => setHoverNode(n || null)}
        onBackgroundClick={() => setActiveNode(null)}
        onZoomEnd={({ k }) => {
          zoomLevelRef.current = k;
        }}
        onEngineStop={() => {
          if (!pendingAutoFitRef.current) return;
          pendingAutoFitRef.current = false;
          fitToView(350);
        }}
        d3VelocityDecay={0.4}
        cooldownTicks={100}
      />
      {hoverNode && (
        <div className="absolute top-4 left-4 bg-surface-elev border border-stone-1 rounded p-2 text-xs text-text shadow pointer-events-none fade-in">
          <div className="font-semibold">{hoverNode.name}</div>
          <div className="text-text-muted capitalize">{hoverNode.group}</div>
        </div>
      )}
    </div>
  );
}
