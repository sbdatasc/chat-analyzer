# Chat Analyzer — Project Brief

A local-first desktop app that turns a user's ChatGPT export into a
semantic knowledge graph, then exposes it through a unified chat + graph
interface. Runs entirely on the user's machine using Ollama.

Target: MacBook Pro M1 class hardware. Single user. Personal use.

---

## 1. Goals & Non-Goals

### Goals
- Import `conversations.json` from ChatGPT exports and cache message
  content locally in compressed form for fast retrieval.
- Extract entities, concepts, topics, and prompt patterns into a
  semantic knowledge graph stored locally.
- Provide a unified "chat with my chats" interface that pairs a text
  answer with a graph of the nodes that produced it.
- Support five built-in lenses: Territory, Drift, Bridges, Orphans, Path.
- Keep all inference local via any OpenAI-compatible endpoint
  (default: Ollama).

### Non-Goals (v1)
- Multi-user accounts or cloud sync.
- Multi-file upload in a single action.
- Contradictions lens (dropped in design).
- Full-canvas force-directed graph browser.
- Persisted chat threads across app restarts.
- Agentic retrieval (model calling back into graph mid-generation).
- OS keychain integration for API keys.
- Reconstruction of attachments (images, files, DALL-E outputs).

---

## 2. Tech Stack (committed)

| Layer | Choice | Notes |
|---|---|---|
| App shell | Tauri 2 | Native M1, small binary, Rust backend |
| Frontend | React + TypeScript + Tailwind | Minimal, no component lib bloat |
| Graph viz | Raw SVG or a library under 25kb gzipped | Mini-graph only, no full canvas |
| Backend | Rust | Runs in Tauri process |
| Database | SQLite (via `rusqlite`) | Single file, WAL mode |
| Vector index | `sqlite-vec` extension | HNSW, 768-dim embeddings |
| Full-text | FTS5 (built into SQLite) | No extension needed |
| LLM client | OpenAI-compatible HTTP | Via `reqwest` |
| Default LLM runtime | Ollama at `http://localhost:11434/v1` | User-configurable |
| Default embedding model | `nomic-embed-text` | 768-dim |
| Default chat model | `qwen2.5:7b` | Balanced tier |
| JSON parsing | `serde_json` with streaming for large files | |
| Clustering (manual trigger) | HDBSCAN via Rust crate or Python sidecar | |

### Rationale
- SQLite + sqlite-vec + FTS5 is the fastest embedded option on M1 and
  the most durable (Kuzu was archived October 2025; avoid).
- Tauri keeps install footprint small and avoids JVM/Electron bloat.
- OpenAI-compatible abstraction means swapping Ollama for LM Studio or
  llama.cpp server is a config change, not a code change.

---

## 3. User Flows

### Flow 1 — Ingest
User uploads one `conversations.json` at a time. App validates, moves
the file into an internal `sources/` directory, scans it, and runs
async extraction to populate the knowledge graph. Low-confidence
extractions are parked for review in the Mapping Workbench.

### Flow 2 — Chat + Graph (unified)
One screen with a text input and five lens chips. Every interaction
produces two panels side-by-side:
- Left (60%): text answer with inline `[^n]` citations, session history
- Right (40%): mini-graph of the nodes referenced in the answer

Hovering text citations highlights graph nodes and vice versa.
Clicking any node re-centers the graph and re-queries the text panel.

### Flow 3 — Mapping Workbench (secondary surface)
Reached from any node's "Trace" action. Shows source messages side-by-
side with graph contributions. User can accept/reject/edit/merge/promote
span/add edge. Primary surface for curating <0.60 confidence items.

---

## 4. Workspace Layout

User picks a workspace folder on first launch (default
`~/ChatAnalyzer`). Structure is created automatically.

```
<workspace-root>/
├── sources/                            # App-managed, user does not touch
│   └── conversations-YYYY-MM-DD-{hash6}.json
├── state/
│   ├── app.sqlite                      # All graph, index, FTS, manifest
│   └── manifest.jsonl                  # Per-run log, retained forever
└── workspace.yaml                      # Editable config
```

Global app config (recent workspaces, default endpoint) lives in
`~/Library/Application Support/chat-analyzer/settings.yaml`.

---

## 5. Configuration (`workspace.yaml`)

```yaml
endpoints:
  default:
    base_url: http://localhost:11434/v1
    api_key: ""
    label: "Local Ollama"

jobs:
  extraction:
    endpoint: default
    model: qwen2.5:7b
    temperature: 0.2
    timeout_s: 120
    max_retries: 2
  embedding:
    endpoint: default
    model: nomic-embed-text
    timeout_s: 30
    max_retries: 2
  triage_preview:
    endpoint: default
    model: qwen2.5:3b
    temperature: 0.0
    timeout_s: 30
  evaluation:
    endpoint: default
    model: qwen2.5:7b
    temperature: 0.0
    timeout_s: 60

ui:
  split_ratio: 0.6        # text-left, graph-right
  graph_node_cap: 25
  focus_stack_size: 4
```

API keys stored as plain text. Acceptable for single-user local tool.

### Preset tiers (offered in UI)

| Preset | Triage | Extraction | Embedding | Evaluation |
|---|---|---|---|---|
| Fast | qwen2.5:3b | qwen2.5:3b | nomic-embed-text | qwen2.5:3b |
| Balanced | qwen2.5:3b | qwen2.5:7b | nomic-embed-text | qwen2.5:7b |
| Deep | qwen2.5:7b | qwen2.5:14b | mxbai-embed-large | qwen2.5:14b |

---

## 6. SQLite Schema (full DDL)

Enable WAL, foreign keys, and sqlite-vec extension on open.

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous = NORMAL;

-- Source files registry
CREATE TABLE source_registry (
  source_id          TEXT PRIMARY KEY,      -- sha256 of file contents
  internal_filename  TEXT NOT NULL UNIQUE,  -- filename inside sources/
  file_size          INTEGER NOT NULL,
  export_date        TEXT,
  imported_at        TEXT NOT NULL,
  conversation_count INTEGER NOT NULL DEFAULT 0,
  original_filename  TEXT
);

-- Message-level index (pointers, NOT content)
CREATE TABLE message_index (
  conversation_id  TEXT NOT NULL,
  message_id       TEXT NOT NULL,
  parent_id        TEXT,
  role             TEXT NOT NULL,
  model            TEXT,
  create_time      REAL,
  byte_start       INTEGER NOT NULL,
  byte_length      INTEGER NOT NULL,
  source_id        TEXT NOT NULL,
  on_visible_path  INTEGER NOT NULL DEFAULT 0,
  is_system        INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (conversation_id, message_id),
  FOREIGN KEY (source_id) REFERENCES source_registry(source_id)
);
CREATE INDEX idx_msg_conv ON message_index(conversation_id);
CREATE INDEX idx_msg_src ON message_index(source_id);

-- Multi-source presence tracking
CREATE TABLE conversation_sources (
  conversation_id  TEXT NOT NULL,
  source_id        TEXT NOT NULL,
  update_time      REAL,
  turn_count       INTEGER,
  is_canonical     INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (conversation_id, source_id),
  FOREIGN KEY (source_id) REFERENCES source_registry(source_id)
);

-- Graph nodes (all types in one table)
CREATE TABLE node (
  id        TEXT PRIMARY KEY,
  type      TEXT NOT NULL,          -- conversation | entity | concept | topic | pattern
  name      TEXT NOT NULL,
  props     TEXT,                   -- JSON blob
  created   TEXT NOT NULL,
  updated   TEXT NOT NULL
);
CREATE INDEX idx_node_type ON node(type);
CREATE INDEX idx_node_name ON node(name);

-- Graph edges
CREATE TABLE edge (
  id        INTEGER PRIMARY KEY AUTOINCREMENT,
  src_id    TEXT NOT NULL,
  dst_id    TEXT NOT NULL,
  type      TEXT NOT NULL,          -- mentions | discusses | belongs_to | uses_pattern | relates_to | similar_to
  props     TEXT,                   -- JSON blob
  FOREIGN KEY (src_id) REFERENCES node(id) ON DELETE CASCADE,
  FOREIGN KEY (dst_id) REFERENCES node(id) ON DELETE CASCADE,
  UNIQUE (src_id, dst_id, type)
);
CREATE INDEX idx_edge_src ON edge(src_id, type);
CREATE INDEX idx_edge_dst ON edge(dst_id, type);

-- Extraction tracking per conversation
CREATE TABLE extraction_state (
  conversation_id  TEXT PRIMARY KEY,
  status           TEXT NOT NULL,   -- pending | processing | done | failed | skipped
  content_hash     TEXT NOT NULL,
  prompt_version   TEXT NOT NULL,
  model_used       TEXT,
  started_at       TEXT,
  completed_at     TEXT,
  error            TEXT,
  FOREIGN KEY (conversation_id) REFERENCES node(id)
);

-- Per-conversation parked items (<0.60 confidence, awaiting Workbench review)
CREATE TABLE parked_extractions (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id  TEXT NOT NULL,
  kind             TEXT NOT NULL,   -- entity | concept | topic | pattern
  payload          TEXT NOT NULL,   -- JSON of proposed node+edge
  confidence       REAL NOT NULL,
  created_at       TEXT NOT NULL,
  FOREIGN KEY (conversation_id) REFERENCES node(id)
);

-- Run manifest (append-only, retained forever)
CREATE TABLE manifest (
  run_id      TEXT PRIMARY KEY,
  ts          TEXT NOT NULL,
  kind        TEXT NOT NULL,        -- ingest | extract | maint
  parsed      INTEGER,
  skipped     INTEGER,
  errors      INTEGER,
  notes       TEXT
);

-- Saved snapshots (lens + filter state for bookmarking)
CREATE TABLE snapshots (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT NOT NULL,
  state      TEXT NOT NULL,         -- JSON
  created_at TEXT NOT NULL
);

-- Editable prompts with revision history
CREATE TABLE prompts (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  kind        TEXT NOT NULL,        -- extraction | planner | answerer
  version     INTEGER NOT NULL,
  body        TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  is_active   INTEGER NOT NULL DEFAULT 0,
  UNIQUE (kind, version)
);

-- Vector index (sqlite-vec virtual table)
CREATE VIRTUAL TABLE node_vec USING vec0(
  node_id  TEXT PRIMARY KEY,
  embedding FLOAT[768]
);

-- Full-text index on node names and descriptions
CREATE VIRTUAL TABLE node_fts USING fts5(
  node_id UNINDEXED,
  name,
  description
);
```

Content rule: message text is NEVER stored in this database. To read
any message, use `message_index.byte_start` + `byte_length` to seek
into `sources/{internal_filename}`.

---

## 7. Ingest Pipeline

### Stages

| # | Stage | Sync/Async | Purpose |
|---|---|---|---|
| 1 | Validate | sync | JSON shape + checksum dedup vs `source_registry` |
| 2 | Register | sync | Move file into `sources/`, insert `source_registry` row |
| 3 | Scan | sync | Stream-parse, populate `message_index`, seed Conversation nodes |
| 4 | Extract | async | Ollama extraction per conversation, per-conversation failures do not halt |
| 5 | Semantic dedup | async | sqlite-vec match against existing Entity/Concept nodes |
| 6 | Apply | async | >=0.85 solid, 0.60-0.85 amber, <0.60 parked |
| 7 | Finalize | sync | Mark status, append manifest |

### Validate rules
- Must be valid JSON
- Root must be a list of objects each with `mapping` and `current_node`
- Reject on shape mismatch with a clear error
- Compute sha256 of file contents
- Reject if checksum exists in `source_registry` with "Already imported on {date}"

### Register rules
- Move (not copy) file from upload location into `sources/`
- Rename to `conversations-{export_date}-{hash6}.json`
  - `export_date` inferred from newest conversation's `update_time`, else file mtime, else today
  - `hash6` is first 6 chars of file sha256
- Preserve `original_filename` in `source_registry` for UI display

### Scan rules
- Use streaming JSON parse (don't load whole file into memory)
- For each conversation:
  - Walk the mapping tree from `current_node` back to root via `parent` pointers, marking visible path
  - Record every message in `message_index` with:
    - `byte_start`, `byte_length` pointing into source file
    - `on_visible_path = 1` for visible-path messages, else 0
    - `is_system = 1` for role=system, else 0
  - Skip role=system messages when rendering but index them
  - Upsert Conversation node in `node` table with:
    - `id` = conversation_id
    - `type` = 'conversation'
    - `props` = JSON with {title, chatgpt_created, chatgpt_updated, turn_count, branch_count, models, has_attachments, incomplete, content_hash}
  - Upsert `conversation_sources` row; update `is_canonical` if this version's `update_time` is newest
  - If `content_hash` changed vs existing, mark `extraction_state.status = 'stale'`

### Edge cases during scan
| Case | Behavior |
|---|---|
| 0 non-system messages | Skip, increment skipped count |
| User turns only, no assistant | Index with `incomplete=true` |
| Orphan messages (no path to root) | Skip, log warning |
| Invalid UTF-8 | Replace char, warn in manifest |
| Conversation marked deleted | Skip |

### Extract rules
- One conversation at a time (no parallelism in v1)
- Token estimation before send:
  - If under 75% of model context window, send whole
  - If over, chunk by message boundaries with 1-message overlap, extract per chunk, merge extractions
- Use JSON mode (`response_format: {"type": "json_object"}`) if endpoint probe succeeded
- Else append "Return ONLY valid JSON, no prose" to prompt and parse tolerantly
- On parse failure after `max_retries`, mark `extraction_state.status='failed'` with error, move on

### Semantic dedup rules
- For each proposed Entity or Concept:
  - Compute embedding via embedding job's model
  - Query `node_vec` for top-3 nearest neighbors of same type
  - If any neighbor has cosine similarity >= 0.92, MERGE: use existing node, don't create new
  - Merging behavior: extend `updated` timestamp, append to any aggregate counters in `props`, keep existing `name`
  - Else create new node, embed, insert into `node_vec` and `node_fts`
- For Topics: same logic but threshold 0.88 (topics are coarser)
- Prompt Patterns: exact-name match only, no embedding dedup

### Apply thresholds
| Confidence | Action |
|---|---|
| >= 0.85 | Create node + edge immediately (solid) |
| 0.60 – 0.849 | Create node + edge, flag amber in UI |
| < 0.60 | Insert into `parked_extractions`, do NOT modify graph |

### Idempotency rules
- Same (Conversation, Entity/Concept) edge: upsert, update `count` in edge `props`, append new `message_ids` (cap 20 samples)
- Re-extraction after `content_hash` change: delete prior edges FROM that conversation, rebuild. Nodes survive if other conversations support them.
- Never delete a node automatically. Orphan nodes (zero edges) stay unless user deletes.

---

## 8. Flow 2 — Chat + Graph UI

### Layout

```
+--------------------------------------------------------+
|  Chat Analyzer                       [Ingest]  [Chat]  |
+--------------------------------------------------------+
|  [Type a question...]                                  |
|  [Territory] [Drift] [Bridges] [Orphans] [Path]        |
|  [New Chat] [Snapshots]                                |
+------------------------------+-------------------------+
|                              |                         |
|  TEXT PANEL (60%)            |  GRAPH PANEL (40%)      |
|                              |                         |
|  Session history above       |  Mini-graph of nodes    |
|  Current answer with [^n]    |  referenced in current  |
|  Citations list below        |  answer. Max 25 nodes.  |
|                              |                         |
+------------------------------+-------------------------+
```

Divider is draggable, ratio saved per workspace.

### Home view (before any query)
Show five tiles:
1. **Recently added** — 5 most recent Concepts from last ingest
2. **Growing** — 3 topics with biggest month-over-month growth
3. **Orphan count** — "N concepts untouched since {date}"
4. **Ask the graph** — prompt to start a query
5. **Snapshots** — saved bookmarks

Click any tile to load a lens or start a new query.

### Every turn produces both panels
Free-text query -> retrieval planner classifies -> retrieval -> answerer
generates text with `[^n]` citations -> graph panel renders the set of
nodes referenced by those citations, plus 1-hop neighbors (cap 25 nodes).

Lens chip click -> bypasses NL parsing, runs lens canonical query -> same
output shape.

### Bidirectional hover
- Hover a `[^n]` in text -> corresponding node pulses in graph
- Hover a node in graph -> passages contributing to it highlight in text
- Click a node -> graph re-centers on that node, text re-queries around it
- Back button restores prior turn (both panels)

### Graph panel rules
- Max 25 nodes. If query would return more, cluster smallest groups into "+ N more" pills
- 1-hop expansion from direct hits only
- Nodes colored by type: Conversation, Entity, Concept, Topic, Pattern
- Edges labeled by type
- Use raw SVG with force layout or a library under 25kb. Do NOT use
  Cytoscape, D3 force module, or vis.js.

---

## 9. Retrieval Pipeline (per chat turn)

```
User question
     |
     v
Retrieval planner (triage_preview model)
     |
     v
Classified: factual | synthesis | time_based
Key terms + date range extracted
     |
     v
Parallel retrieval:
  - FTS5 query over node_fts -> top 30
  - Vector query over node_vec (using embedded query) -> top 30
     |
     v
Reciprocal Rank Fusion (k=60) -> top 15 nodes
     |
     v
Graph expansion: 1-hop neighbors of top 15 -> cap 25 total
     |
     v
Context assembly:
  - For each Conversation node, read snippets from the local message cache
  - Rank chunks by fused score
  - Pack up to context budget (see below)
     |
     v
Answerer call -> text with [^n] markers
     |
     v
Render both panels
```

### RRF formula
```
score(node) = sum over retrievers [ 1 / (k + rank) ]    k=60
```

### Context budgets

| Model context | Retrieval | Session history | System | Response |
|---|---|---|---|---|
| 8k | 4k tokens | 1k | 1k | 2k |
| 32k | 16k | 2k | 1k | 4k |

Anything dropped for budget reasons is listed below the response as
"also relevant but not included."

### Citation IDs
- Each context chunk gets a sequential `[^n]` ID
- Chunk metadata: `conversation_id`, `message_id`, short snippet
- When user clicks a citation: open source conversation at that turn
  via byte-offset read

---

## 10. Session Memory / Context Retention

Every turn stores four things in memory:

| Component | Content | Purpose |
|---|---|---|
| Text history | Last 5 Q/A pairs verbatim | Natural follow-ups |
| Turn summary | One-line compression of older turns | Coherence without token bloat |
| Active graph frame | Node IDs currently in right panel + types | Pronoun resolution |
| Focus stack | Last 4 nodes hovered or clicked | Disambiguate "it", "that" |

### Prompt sections passed to planner AND answerer
1. Active frame (top — resolves deictic references)
2. Session summary
3. Last 5 turns verbatim
4. Current question

### Topic shift detection
If the new question shares zero key terms with last turn AND contains
no pronouns referring to prior turns: treat as new thread.
- Clear active frame
- Keep history (user may navigate back)

User can force reset with "New Chat" button. Closing app clears all
session state (no persistence in v1).

---

## 11. The Five Lenses

Each lens is a preset query. SQL produces the structural truth; a
small internal prompt narrates it. If the LLM narration fails or is
poor, the SQL output renders directly as an acceptable fallback.

### Lens 1 — Territory
**Question:** Where has my cognitive time gone?

**SQL:**
```sql
SELECT
  t.id, t.name,
  COUNT(DISTINCT e.src_id) AS conv_count,
  MIN(c.props->>'$.chatgpt_created') AS first_seen,
  MAX(c.props->>'$.chatgpt_updated') AS last_seen
FROM node t
JOIN edge e ON e.dst_id = t.id AND e.type = 'belongs_to'
JOIN node c ON c.id = e.src_id AND c.type = 'conversation'
WHERE t.type = 'topic'
GROUP BY t.id
ORDER BY conv_count DESC
LIMIT 20;
```

Plus 6-month sparkline per topic (monthly conversation count).
Plus top 5 concepts per topic (via bridging query).

**Fallback UI:** table with sparklines.
**With LLM:** 150-word narration of dominant territories, growth, and
depth vs breadth signals.

### Lens 2 — Drift
**Question:** How has my thinking shifted over time?

**SQL:** Monthly volume per topic for last 12 months.
```sql
SELECT
  t.name AS topic,
  strftime('%Y-%m', c.props->>'$.chatgpt_created') AS month,
  COUNT(DISTINCT c.id) AS vol
FROM node t
JOIN edge e ON e.dst_id = t.id AND e.type = 'belongs_to'
JOIN node c ON c.id = e.src_id AND c.type = 'conversation'
WHERE t.type = 'topic'
  AND c.props->>'$.chatgpt_created' >= date('now', '-12 months')
GROUP BY t.id, month
ORDER BY month, topic;
```

**Fallback UI:** heatmap grid (topics x months).
**With LLM:** prose naming burned-through, compounding, dormant-revived,
and context-switch patterns.

### Lens 3 — Bridges
**Question:** Which concepts connect otherwise-separate topics?

**Algorithm:**
1. Build bipartite map of Concept -> Topic via `discusses` + `belongs_to`
2. For each Concept with edges into 3+ distinct Topics, mark as bridge
3. For each bridge, fetch 2 sample conversation snippets per crossed topic

**Fallback UI:** raw bridge list with cross-cluster edge counts.
**With LLM:** one line per bridge classifying REAL / LAZY / UNCLEAR
with citation.

### Lens 4 — Orphans
**Question:** What did I touch once and never return to?

**SQL:**
```sql
SELECT c.id, c.name, c.created, e.src_id AS sole_conversation
FROM node c
JOIN edge e ON e.dst_id = c.id AND e.type = 'discusses'
WHERE c.type = 'concept'
  AND c.created < date('now', '-90 days')
GROUP BY c.id
HAVING COUNT(DISTINCT e.src_id) = 1
ORDER BY c.created DESC;
```

**UI actions per row:** Promote (flag for revisit) | Archive (hide from
future scans). Store both as JSON props on the node.

**Fallback UI:** list sorted by age.
**With LLM:** HIGH/MEDIUM/LOW revisit rating, plus single revival
question for HIGH items.

### Lens 5 — Path
**Question:** How is concept A related to concept B?

**Algorithm:**
1. Bidirectional BFS from A and B, max depth 3
2. For each path, compute strength = min(edge_conversation_count)
3. Rank paths by strength descending

**Fallback UI:** ordered list of paths with node chain and strength.
**With LLM:** 3-5 sentence narrative of strongest path, labeled STRONG
or THIN.

---

## 12. Mapping Workbench

Reached from any node's "Trace" action, or from the parked-items queue.

### Layout
Two-pane, scoped to a single conversation.

```
+------------------------+------------------------+
| SOURCE (left)          | GRAPH CONTRIBUTIONS    |
|                        | (right)                |
| Messages rendered      | Nodes created by this  |
| from the local cache,  | conversation, edges    |
| each highlightable     | attached. Pending      |
|                        | items shown separately |
+------------------------+------------------------+
```

### Interactions
| Action | Behavior |
|---|---|
| Hover message span | Right pane highlights attached nodes/edges |
| Hover node | Left pane highlights contributing messages |
| Accept pending | Promotes parked item into graph |
| Reject | Removes node contribution from this conversation. Node survives if other conversations support it. |
| Edit name/type | In-place edit of node metadata |
| Merge | Drag node A onto B. Merges embeddings (centroid), re-points edges. |
| Promote span to node | Select text -> right-click -> Create Entity/Concept/Pattern |
| Add edge | Drag between two nodes, pick edge type |

All edits update `node`, `edge`, `node_vec`, `node_fts` atomically.

---

## 13. Prompts

Three are user-editable with revision history (stored in `prompts` table).
Five lens prompts are hardcoded in app source (not user-editable) — poor
narration is acceptable because SQL fallback always renders.

### 13.1 Extraction prompt (EDITABLE)

```
ROLE
You are an extraction engine for a personal knowledge graph. You read ONE
conversation between a user and an assistant, and you emit structured knowledge.

DEFINITIONS
- Entity: a proper noun or named thing (person, tool, company, book, tech).
  Examples: "Ollama", "Andrej Karpathy", "GPT-4".
- Concept: an atomic idea or technique worth remembering.
  Examples: "hybrid search", "knowledge compounding", "recursive CTEs".
- Topic: the high-level subject area. One to three max per conversation.
- PromptPattern: how the user framed a question in a reusable way.
  Examples: "tradeoff comparison", "devil's advocate check", "decision breakdown".

RULES
- Emit only what is DISCUSSED substantively. Passing mentions do not count.
- Normalize names: lowercase, kebab-case, no punctuation.
  "GPT-4" becomes "gpt-4". "Andrej Karpathy" becomes "andrej-karpathy".
- Confidence scale: 0.9+ for items defined or discussed at length.
  0.6 to 0.85 for clear mentions. Below 0.6 for uncertain.
- For each item, cite 1 to 3 message_ids from the conversation that support it.
- Caps per conversation: 15 concepts, 10 entities, 3 topics, 3 patterns.
  Quality over volume.

INPUT
Title: {{title}}
Created: {{created}}
Messages:
{{messages_with_ids}}

OUTPUT (valid JSON only, no prose):
{
  "topics": [{"name": "", "confidence": 0.0, "message_ids": []}],
  "entities": [{"name": "", "type": "person|tool|org|book|tech|place",
                "description": "", "confidence": 0.0, "message_ids": []}],
  "concepts": [{"name": "", "description": "", "confidence": 0.0,
                "message_ids": []}],
  "prompt_patterns": [{"name": "", "description": "", "confidence": 0.0,
                       "message_ids": []}]
}
```

### 13.2 Retrieval planner (EDITABLE)

```
ROLE
You are a query router for a personal knowledge graph.

TASK
Classify the user's question as ONE of:
- factual: asking for a specific fact from past conversations
- synthesis: asking to connect or compare across multiple topics
- time_based: asking about a period, timeline, or chronology

Also extract:
- key_terms: 2 to 5 content words for search
- date_range: {"start": "YYYY-MM", "end": "YYYY-MM"} if time bounds present, else null

ACTIVE FRAME (what user is looking at):
{{active_frame}}

SESSION HISTORY (last 5 turns):
{{recent_turns}}

QUESTION: {{user_question}}

OUTPUT (JSON only):
{"class": "", "key_terms": [], "date_range": null}
```

### 13.3 Grounded answerer (EDITABLE)

```
ROLE
You answer questions about the user's own past ChatGPT conversations using ONLY
the CONTEXT provided. You do not use outside knowledge.

ACTIVE FRAME (what the user is currently looking at):
{{active_frame}}

SESSION SUMMARY (older turns compressed):
{{session_summary}}

LAST 5 TURNS (verbatim):
{{recent_turns}}

CONTEXT (ranked, fetched for this question):
{{context_chunks}}

QUESTION: {{user_question}}

RULES
- Use ACTIVE FRAME to resolve pronouns ("it", "that one").
- Every claim gets a [^n] marker matching CONTEXT source IDs.
- If CONTEXT does not support a claim, say "the archive does not show this".
- Executive tone. Answer first. Under 200 words unless synthesis.
- If the question is a follow-up, start where the last turn left off, do not repeat.

ANSWER:
```

### 13.4 Lens prompts (HARDCODED, not user-editable)

Located in `src/prompts/lenses/` in source code. Territory, Drift,
Bridges, Orphans, Path each have a small built-in template taking
SQL-produced data as input. See Section 11 for expected outputs. If
local model output is poor, UI renders from SQL directly.

---

## 14. Endpoint Abstraction

All LLM calls go through an `LlmClient` trait:

```rust
#[async_trait]
pub trait LlmClient {
    async fn chat(&self, model: &str, messages: Vec<Message>,
                  opts: ChatOpts) -> Result<ChatResponse>;
    async fn embed(&self, model: &str, input: &str) -> Result<Vec<f32>>;
    async fn list_models(&self) -> Result<Vec<String>>;
    async fn health(&self) -> Result<HealthStatus>;
}
```

Default implementation: `OpenAiCompatibleClient` targeting any URL
that implements `/v1/chat/completions`, `/v1/embeddings`, `/v1/models`.

### Health probes on endpoint save
1. `GET /v1/models` -> 200 with list
2. `POST /v1/chat/completions` with "ping" -> completes
3. `POST /v1/embeddings` with "ping" -> returns vector

UI shows green/amber/red per check. Amber means chat works but not
embeddings (or vice versa); the UI disables incompatible job assignments.

### JSON mode detection
On first save of an endpoint, probe once with `response_format:
{"type": "json_object"}`. If accepted, flag in settings as
`supports_json_mode = true`. Extraction calls use JSON mode if
available, else fall back to prompt-level coercion.

### Retry policy
- 3 attempts per call
- Exponential backoff: 1s, 3s, 9s
- Timeout per call from job config (default 120s for extraction)
- Timeout counts as one retry cycle
- After all retries, propagate failure but do not halt outer pipeline

---

## 15. Error Handling

| Failure | Behavior |
|---|---|
| Invalid JSON on import | Reject with user-visible error. Move nothing. |
| Source file moved after registration | Validate on launch. Warn. Offer to re-register. |
| Extraction fails for a conversation | Mark `extraction_state.status='failed'`, store error, continue pipeline |
| LLM returns malformed JSON | 3 parse attempts with strict prompt. Then fail the item. |
| Embedding endpoint down mid-ingest | Pause pipeline, surface in UI, resume on next launch |
| User deletes a source file externally | `message_index` rows become unreadable. Flag in UI. Graph survives. |
| SQLite corruption | Refuse to start. Show diagnostic. Offer to back up and rebuild. |

All unhandled errors logged to `state/manifest.jsonl` with
`kind: "error"` and full context.

---

## 16. Build Order (milestones with acceptance tests)

Ship order matters more than feature count. Do NOT skip milestones.

### M1 — Repo scaffolding
- Tauri 2 app skeleton with React + TypeScript + Tailwind frontend
- Rust backend with `rusqlite` + `sqlite-vec` linked
- Workspace folder picker on first launch
- SQLite init creates full schema from Section 6

**Acceptance:** `cargo run` opens a window, workspace selected,
`state/app.sqlite` created with all tables. No crashes.

### M2 — Ingest scan stage
- File picker for `conversations.json`
- Validate + move to `sources/` + register
- Stream-parse builds `message_index` and Conversation nodes
- Manifest row written

**Acceptance:** Upload a real export. SQLite shows `source_registry`
row, `message_index` populated, N Conversation nodes in `node` table.
File appears in `sources/` with renamed filename. Re-uploading same
file is rejected with "already imported" message.

### M3 — Ollama client + extraction (happy path)
- `LlmClient` trait + `OpenAiCompatibleClient` implementation
- Settings UI for endpoint + model picker
- Extraction job that processes ONE conversation
- Writes Entity/Concept/Topic/Pattern nodes and edges with confidence
- Stores embeddings in `node_vec`, names in `node_fts`

**Acceptance:** With Ollama running, after M2 upload, click "Extract
first conversation". Graph gains entity/concept/topic nodes. Confidence
values stored. Edges linked to the Conversation node. Node embeddings
in `node_vec`.

### M4 — Full async extraction pipeline
- Background worker iterates pending conversations
- Per-conversation failure handling
- Semantic dedup at apply time
- Apply thresholds (0.85 / 0.60 / <0.60)
- Parked items land in `parked_extractions`
- Resume-on-launch if interrupted

**Acceptance:** Upload a 100-conversation export. Close app mid-extract.
Reopen. Extraction resumes from where it stopped. At end, graph has
hundreds of nodes, duplicates merged (e.g. one "Ollama" entity, not
multiple), parked items visible in UI count.

### M5 — Chat + Graph UI (free text, no lenses)
- Unified layout with split pane
- Free-text query input
- Retrieval planner + answerer calls
- Text panel with citation markers
- Graph panel with mini-graph (25 node cap)
- Bidirectional hover highlighting
- Node click re-centers + re-queries

**Acceptance:** Ask "what did I discuss about Ollama?" -> text answer
with `[^1]`, `[^2]` citations. Graph panel shows Ollama node and
related concepts. Hover `[^1]` -> graph node pulses. Click a related
concept -> canvas re-centers.

### M6 — Session memory
- Store last 5 turns verbatim
- Compress older turns into summary
- Track active frame (graph panel node IDs)
- Track focus stack (last 4 hovered/clicked)
- Topic shift detection clears frame, keeps history
- "New Chat" button hard-resets

**Acceptance:** Ask about Ollama, then ask "tell me more about that".
Second question resolves "that" to Ollama via active frame. Ask an
unrelated question -> frame clears, history retained, back button restores.

### M7 — Home view + snapshots
- Five tiles: Recently added, Growing, Orphan count, Ask, Snapshots
- Snapshots save lens + filter state as named bookmarks
- Click a tile -> loads relevant lens or opens input

**Acceptance:** Fresh app open shows tiles with real numbers. Save a
snapshot; reopen app; snapshot appears in list; clicking restores
both panels.

### M8 — Lens chips (one at a time)
Ship in this order:
1. Territory (pure SQL, LLM narration optional)
2. Orphans (pure SQL + UI actions)
3. Drift (heatmap UI is the main surface)
4. Bridges (centrality computation)
5. Path (BFS)

**Acceptance per lens:** chip click populates both panels per spec in
Section 11. Each lens works without LLM (fallback UI visible). LLM
narration adds on top when endpoint is healthy.

### M9 — Mapping Workbench
- Side-by-side source + graph contributions view
- Parked items queue
- All interactions per Section 12
- Merge operation updates embeddings and re-points edges

**Acceptance:** Click "Trace" on any node -> Workbench opens scoped to
a conversation. Can accept a parked item; it appears in main graph.
Can merge two concepts; edges re-point, no dangling references.

### M10 — Prompt editing + revision history
- Settings panel shows three editable prompts
- Save creates new row in `prompts` with incremented version
- User can roll back to earlier version
- "Re-extract all with new prompt" button (opt-in)

**Acceptance:** Edit extraction prompt, save, version 2 stored.
Re-extract one conversation with new prompt; `extraction_state.prompt_version`
reflects v2. Roll back to v1; subsequent extractions use v1.

### M11 — Manual maintenance jobs
- Similarity edges (`similar_to`) computed across concepts via embedding
  pairs above threshold
- Topic clustering via HDBSCAN on concept embeddings; new Topic nodes
  created with `BELONGS_TO` edges
- Both manual triggers in a Maintenance menu
- Progress bars, can be cancelled

**Acceptance:** Click "Compute similarity edges". New `similar_to`
edges appear between conceptually-close concepts. Graph panel shows
them on relevant nodes.

### M12 — Polish, packaging, signing
- App icon, menu items, about dialog
- Tauri packaging for macOS
- Signed build
- First-launch tutorial

---

## 17. Directory Structure (repo)

```
chat-analyzer/
├── PROJECT.md                  # This file
├── README.md
├── Cargo.toml                  # Tauri workspace root
├── package.json
├── tauri.conf.json
├── src-tauri/
│   ├── src/
│   │   ├── main.rs
│   │   ├── commands/           # Tauri commands (IPC from frontend)
│   │   ├── db/                 # SQLite access, migrations
│   │   ├── ingest/             # Validate, register, scan, extract
│   │   ├── llm/                # LlmClient trait + impls
│   │   ├── retrieval/          # Planner, RRF, context assembly
│   │   ├── lenses/             # Territory, Drift, Bridges, Orphans, Path
│   │   ├── prompts/            # Embedded prompt templates (lens prompts)
│   │   └── workbench/          # Mapping Workbench logic
│   └── Cargo.toml
├── src/                        # React frontend
│   ├── main.tsx
│   ├── App.tsx
│   ├── pages/
│   │   ├── Home.tsx
│   │   ├── Ingest.tsx
│   │   ├── Chat.tsx
│   │   └── Workbench.tsx
│   ├── components/
│   │   ├── ChatPanel.tsx
│   │   ├── GraphPanel.tsx
│   │   ├── LensChips.tsx
│   │   ├── SnapshotList.tsx
│   │   └── ...
│   ├── hooks/
│   └── styles/
└── tests/
    ├── fixtures/
    │   └── small-export.json   # 5-conversation test export
    └── integration/
```

---

## 18. V2 Parking Lot (explicitly deferred)

- Multi-file upload in one action
- Agentic retrieval (model calls graph mid-generation)
- Persisted chat threads across app sessions
- Full-canvas force-directed graph visualization
- Contradictions lens
- Parallel extraction workers
- OS keychain integration
- Export of views to PDF/image
- Custom Cypher-style query editor
- Collaborative/multi-user mode
- Mobile companion app

---

## 19. Anti-patterns (what NOT to do)

- Do NOT send or sync message content outside the local workspace.
- Do NOT keep duplicate raw and compressed copies of message content.
- Do NOT use Kuzu — archived October 2025, storage format was never stable.
- Do NOT use Electron — M1 minimalism requires Tauri.
- Do NOT use Cytoscape, D3 force module, or vis.js — too heavy for the
  25-node mini-graph. Raw SVG or a <25kb library.
- Do NOT introduce a second inference runtime alongside Ollama.
  One runtime, abstracted through OpenAI-compatible endpoints.
- Do NOT parallelize extraction in v1. One conversation at a time.
- Do NOT auto-delete orphan nodes. User explicit action only.
- Do NOT persist chat threads. Session memory is in-process only.
- Do NOT store API keys in OS keychain in v1. Plain text in workspace.yaml.
- Do NOT reconstruct missing attachments. Show placeholder only.
- Do NOT add features before M5 ships and is usable end-to-end.

---

## 20. Definition of Done

v1 is done when:
1. User can upload a `conversations.json`, watch extraction complete,
   and see their knowledge graph populated.
2. User can ask natural language questions and receive grounded answers
   with citations and a paired mini-graph, with session memory working.
3. All five lens chips produce useful output (SQL fallback acceptable
   even if LLM narration is poor).
4. Mapping Workbench lets user curate parked extractions.
5. User can edit any of the three exposed prompts and see improvements.
6. App runs on macOS M1 with under 500MB resident memory for a
   1000-conversation archive.
7. All message content stays local to the workspace with no remote retention.
