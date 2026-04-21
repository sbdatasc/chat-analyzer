# Knowledge Graph 2.0 Architecture Plan

To transform the static Knowledge Graph into a native, hyper-interactive "superuseful" tool for exploring your ChatGPT history, we will rip out the static SVG radial layout and upgrade to a fully simulated, hyper-scalable physics canvas via `react-force-graph-2d`.

## Proposed Architecture

### 1. Engine Migration (`GraphCanvas.tsx`)
Currently, `GraphCanvas` manually plots nodes in a fixed geometric circle using raw SVG elements. It cannot pan, zoom, or simulate mass.
- We will install `react-force-graph-2d` to bridge native HTML5 Canvas physics into the interface.
- This will unlock **infinite panning, scrolling zoom, collision logic, and animated node settling algorithms (D3 Force Simulation)**.

### 2. Node Physics & Visuals
To make the graph easily readable when it scales to thousands of nodes, we will implement dynamic Semantic rendering:
- **Node Coloring**: Nodes will automatically inherit `BRANDING.md` token colors based on their type (e.g. `conversation` is a gray hub, `topics` are Accent, `concepts` are muted).
- **Node Sizing (Mass)**: Nodes with higher incoming connection weights (e.g., heavily used topics/patterns) will natively grow larger and carry more gravitational mass.
- **Link Particles**: Edges between nodes will emit tiny, traveling light particles so you can visualize the directional flow of ideas from your conversations to abstract topics.

### 3. Deep Interaction (Hover, Click & Highlight)
- **Tooltips**: Hovering over any node will draw a native canvas tooltip displaying its name alongside any extracted AI `description` (e.g. knowing what an obscure entity represents without clicking it).
- **Focus Highlighting**: Clicking on a `Topic` or a `Pattern` will seamlessly fade out the rest of the canvas and aggressively highlight *only* the immediate neighborhood of direct conversation nodes it supports and any related concepts! Click background to reset.

### 4. Side-Panel Insights
- Currently, clicking a Conversation node pops up the `MappingWorkbench`. But clicking an Entity shouldn't just be hollow. We will add an `onNodeClick` handler that pops up a right-aligned Sidebar Inspector displaying the node's `tier`, `confidence`, and `description`, along with a list of linked interactions.

## User Feedback Required
This replaces our entire manual SVG graphing approach with a physics-based, scalable canvas library perfectly suited for Electron/Tauri scale. Does this visual design scheme match your expectations for "superuseful"? If you'd like to use a different library (like pure D3) or you have specific physics rules you want adjusted, let me know!
