# Cairn — Brand & Visual System

Companion document to `PROJECT.md`. Defines the brand identity, visual
system, and integration instructions for the build. Antigravity should
treat this as binding for any user-facing surface.

---

## 1. Name and concept

**App name:** Cairn

A cairn is a pile of stones travelers stack to mark a trail. Each stone
is small; the pile accumulates meaning; others find their way because
someone before them paused to place a marker. That is what this app does
with a ChatGPT archive: turns scattered conversations into a trail you
can navigate.

**Tagline:** A trail through your own thinking.

**Usage of name:**
- App title in all surfaces: "Cairn"
- Never "Cairn App", "Cairn.app", or "CairnApp"
- Package identifiers: `com.cairn.desktop` (bundle), `cairn` (Cargo,
  npm)
- In prose: capitalized, no italics

---

## 2. Color palette

One accent. Everything else lives in a stone scale. CSS custom
properties, switched by `prefers-color-scheme`.

### Light mode
```css
--surface:       #F7F4EE;   /* warm paper, never pure white */
--surface-elev:  #FFFFFF;
--text:          #1A1918;   /* ink */
--text-muted:    #6B655D;
--stone-1:       #E8E2D6;
--stone-2:       #C4BDB1;
--stone-3:       #544F48;
--accent:        #C5894B;   /* ochre */
--accent-soft:   #E8C89B;
--flag-amber:    #D4941F;   /* parked / amber extractions */
--flag-red:      #B5432B;   /* errors */
--flag-green:    #5A7C4A;   /* healthy status */
```

### Dark mode
```css
--surface:       #14130F;
--surface-elev:  #1E1C18;
--text:          #E8E3D7;
--text-muted:    #8B847B;
--stone-1:       #2E2B27;
--stone-2:       #544F48;
--stone-3:       #C4BDB1;
--accent:        #D4A066;   /* ochre shifted for dark bg */
--accent-soft:   #4A3B24;
--flag-amber:    #E0A437;
--flag-red:      #C85842;
--flag-green:    #6F9460;
```

### Usage rules
- `--accent` only on: citations, active lens chip, selected graph node,
  primary button, focus ring.
- Everything else uses the stone scale.
- Never place two accent-colored items adjacent. One accent per area of
  focus.

---

## 3. Typography

Free, self-hosted, distinctive.

- **UI sans:** IBM Plex Sans
- **Mono:** IBM Plex Mono (citations, code, byte-offset previews)
- No serif, no additional families

### Weights in use
- 400 body
- 500 labels and UI elements
- 600 headers

No 700 or heavier. No italic. Single-family keeps the app quiet.

### Type scale (rem)
```
0.75   meta/caption
0.875  UI small
1      body default
1.125  emphasized body
1.25   small header
1.5    medium header
2      large header (rare)
```

Nothing bigger than 2rem. This is a tool, not a marketing site.

### Numeric rendering
All numbers in tables, counts, and timestamps use tabular figures:
```css
font-feature-settings: "tnum", "cv01";
```

---

## 4. Logo mark

Three stacked rounded shapes ascending toward the top. Imperfect
alignment suggests a human hand.

### SVG specification
- Canvas: 512 × 512
- Safe padding: 64px
- Three shapes stacked bottom to top with 8px vertical gap:

| Shape  | Width | Height | Corner Radius | X Offset | Rotation |
|--------|-------|--------|---------------|----------|----------|
| Bottom | 320   | 96     | 20            | center   | 0°       |
| Middle | 240   | 88     | 18            | +6       | -1°      |
| Top    | 160   | 80     | 16            | -6       | +1.5°    |

- Fill color: `--accent` (light mode ochre) or `--accent` (dark mode
  ochre-shifted)
- Background: `--surface`
- No stroke, no shadow on the mark
- macOS rounded-rectangle mask applied at export time

### Exports required
- `cairn-mark.svg` (master)
- `cairn-mark-light.png` 1024×1024
- `cairn-mark-dark.png` 1024×1024
- `.icns` bundle for macOS
- App icon variants: 16, 32, 64, 128, 256, 512, 1024 PNG
- Favicon 32×32 and 64×64

All generated from the SVG master via `tauri icon`.

### Wordmark
"Cairn" set in IBM Plex Sans 500, tracking `-0.01em`, color `--text`.
Mark and wordmark always sit on the same baseline, mark left, wordmark
right, gap equal to wordmark cap height.

---

## 5. Node-type styling (graph panel)

Visual distinction without labels cluttering the canvas.

| Node Type    | Shape            | Fill        | Border        | Notes              |
|--------------|------------------|-------------|---------------|--------------------|
| Conversation | rounded rect     | --stone-2   | none          | 20px radius        |
| Entity       | circle           | --stone-3   | none          |                    |
| Concept      | rounded square   | --stone-1   | 1px --accent  | 8px radius         |
| Topic        | larger rounded   | --accent-soft| none         | 28px radius        |
| Pattern      | rounded rect     | --stone-2   | 1px dashed --stone-3 |              |

### Selected node (any type)
- Fill: `--accent`
- Border: 2px solid `--accent`
- Subtle 2px shadow at 10% opacity
- Text inside: `--surface`

### Edges
- Color: `--stone-2`
- Stroke width: 1px default, 2px for edges with high conversation
  backing (>5)
- Dashed for `similar_to` edges
- Labels appear on hover only, in `--text-muted`, 0.75rem Plex Mono

---

## 6. Voice and copy rules

Binding for every user-facing string.

### Tone
- Executive. Short sentences. No preamble.
- Numbers over adjectives.
- Action labels are verbs: "Import", "Extract", "Trace". Not "Get
  started", "Let's go".

### Punctuation
- Do NOT use em-dashes (—) in UI copy. Use periods or colons.
- No exclamation points anywhere.
- Oxford comma required.

### Banned words
Reject in PR review if found in any user-facing string:

> seamlessly, effortlessly, powerful, unleash, magic, magical, journey,
> unlock, elevate, supercharged, blazing, delightful, beautiful,
> reimagined, revolutionary, next-generation, AI-powered

### Error messages
Format: name the problem, offer one action.

- YES: "File already imported on Apr 15. Choose another export."
- NO:  "Oops! It looks like this file might already exist. Would you
  like to try again?"

### Empty states
Must be useful on first read.

- YES: "No archive yet. Drop a conversations.json to begin."
- NO:  "Welcome to Cairn!"

### Micro-copy examples

| Context               | Copy                                              |
|-----------------------|---------------------------------------------------|
| Import idle           | Drop conversations.json to import                 |
| Import progress       | Scanning 847 conversations                        |
| Extraction running    | 214 of 847 extracted. 12 parked.                  |
| Chat placeholder      | Ask or click a lens                               |
| No results            | No matches in the archive.                        |
| Parked items badge    | 12 parked                                         |
| Healthy endpoint      | Connected                                         |
| Failed endpoint       | Endpoint unreachable. Check base URL.             |
| Snapshot saved        | Saved as "Q3 AI audit"                            |

---

## 7. UI aesthetic principles

Binding. Deviations require explicit approval.

1. **Paper and ink.** Warm off-white surface. Near-black text. No pure
   white, no pure black, no cold grays.
2. **Depth via color, not effects.** No drop shadows beyond 1-2px hair.
   No glassmorphism. No frosted surfaces.
3. **Thin strokes, generous whitespace.** Border radius 6px default,
   10px for large cards. Never fully rounded pills.
4. **One accent, rarely used.** If something is ochre, it earned it.
5. **Motion is quick and subtle.** 120ms ease-out global. No bouncing.
   No staggered entrance animations.
6. **Density is intentional.** Tables tight. Chat breathes.
7. **Graph panel is quiet.** No gridlines, no minimap, no toolbar.
   Nodes and edges on a plain surface.
8. **Follow OS dark/light mode.** No in-app toggle in v1.

---

## 8. Motion

Single global transition variable:

```css
--transition: all 120ms cubic-bezier(0.2, 0, 0, 1);
```

Applied to: hover states, button press, panel divider drag, node
selection.

Disabled under `prefers-reduced-motion: reduce`.

No entrance animations for mounted components. No scroll-reveal. No
parallax.

---

## 9. First-run experience

Single screen.

```
         [mark]
          Cairn

  Choose a folder for your archive.

         [Choose folder]
```

No carousel. No tour. No onboarding modal. The app is simple enough
that the first empty state explains itself after workspace selection.

After workspace is chosen, the home view shows:

```
  No archive yet.
  Drop a conversations.json to begin.
```

Drag target active across the whole window.

---

## 10. Integration instructions for Antigravity

### Tauri / package config

- `tauri.conf.json`: `productName: "Cairn"`, `identifier:
  "com.cairn.desktop"`, update `icon` array with new `.icns` / `.ico`
  paths
- `package.json`: `name: "cairn"`, `description: "A trail through your
  own thinking."`
- `Cargo.toml`: `name = "cairn"`, `version = "0.1.0"`
- Window title: "Cairn"
- About dialog: app name, version (small), nothing else

### Tailwind setup

Install:
```bash
npm install @fontsource/ibm-plex-sans @fontsource/ibm-plex-mono
```

Import the 400/500/600 weights of each in `src/main.tsx` (no italic).

`tailwind.config.js`:
```js
module.exports = {
  content: ['./src/**/*.{ts,tsx,html}'],
  theme: {
    extend: {
      colors: {
        surface: 'var(--surface)',
        'surface-elev': 'var(--surface-elev)',
        text: 'var(--text)',
        'text-muted': 'var(--text-muted)',
        'stone-1': 'var(--stone-1)',
        'stone-2': 'var(--stone-2)',
        'stone-3': 'var(--stone-3)',
        accent: 'var(--accent)',
        'accent-soft': 'var(--accent-soft)',
        'flag-amber': 'var(--flag-amber)',
        'flag-red': 'var(--flag-red)',
        'flag-green': 'var(--flag-green)',
      },
      fontFamily: {
        sans: ['"IBM Plex Sans"', 'system-ui', 'sans-serif'],
        mono: ['"IBM Plex Mono"', 'ui-monospace', 'monospace'],
      },
      borderRadius: {
        DEFAULT: '6px',
        lg: '10px',
      },
      transitionDuration: {
        DEFAULT: '120ms',
      },
      transitionTimingFunction: {
        DEFAULT: 'cubic-bezier(0.2, 0, 0, 1)',
      },
    },
  },
};
```

### Global CSS

`src/styles/globals.css`:
```css
:root {
  --surface: #F7F4EE;
  --surface-elev: #FFFFFF;
  --text: #1A1918;
  --text-muted: #6B655D;
  --stone-1: #E8E2D6;
  --stone-2: #C4BDB1;
  --stone-3: #544F48;
  --accent: #C5894B;
  --accent-soft: #E8C89B;
  --flag-amber: #D4941F;
  --flag-red: #B5432B;
  --flag-green: #5A7C4A;
}

@media (prefers-color-scheme: dark) {
  :root {
    --surface: #14130F;
    --surface-elev: #1E1C18;
    --text: #E8E3D7;
    --text-muted: #8B847B;
    --stone-1: #2E2B27;
    --stone-2: #544F48;
    --stone-3: #C4BDB1;
    --accent: #D4A066;
    --accent-soft: #4A3B24;
    --flag-amber: #E0A437;
    --flag-red: #C85842;
    --flag-green: #6F9460;
  }
}

html {
  font-family: "IBM Plex Sans", system-ui, sans-serif;
  font-feature-settings: "tnum", "cv01";
  background: var(--surface);
  color: var(--text);
}

::selection {
  background: var(--accent-soft);
  color: var(--text);
}

@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    transition-duration: 0ms !important;
    animation-duration: 0ms !important;
  }
}
```

### Icon generation

1. Generate `src-tauri/icons/cairn-mark.svg` per Section 4
2. Run `pnpm tauri icon src-tauri/icons/cairn-mark.svg` to produce all
   platform variants
3. Verify the generated `.icns` renders cleanly at 16px (macOS Finder
   list view)
4. Delete default Tauri icons from the project

### Copy audit as PR requirement

Add to `CONTRIBUTING.md` or PR template:

> Before merging any PR that touches user-facing strings: review against
> BRANDING.md Section 6. Banned words list is a hard reject. Em-dashes
> and exclamation points are hard rejects. Empty states and error
> messages must follow the format rules.

### Assets checklist for M12 (packaging milestone)

- [ ] `cairn-mark.svg` master
- [ ] `.icns` macOS bundle (1024 base)
- [ ] `.ico` Windows bundle (future-proofing, not needed for v1 macOS)
- [ ] `favicon.ico` for any web pages
- [ ] About dialog finalized
- [ ] DMG background (if shipping a DMG): plain surface color with
      mark centered, "Drag Cairn to Applications" in Plex Sans 500

---

## 11. What NOT to do

- No gradient backgrounds anywhere
- No glassmorphism, frosted glass, or translucent surfaces
- No animated logo on launch
- No light/dark mode toggle in UI (follow OS preference)
- No custom scrollbars
- No emoji anywhere in the UI
- No "Powered by" footers
- No version number in main UI (About dialog only)
- No marketing-style landing page inside the app
- No tooltips explaining obvious controls
- No progress spinners without context (always pair with a count or
  label)
- No loading skeletons that mimic content shape (just a subtle pulse on
  `--stone-1`)
