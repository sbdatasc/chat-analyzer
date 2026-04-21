# Contributing to Cairn

## Copy audit (required before merging any PR touching user-facing strings)

Review the change against `BRANDING.md` Section 6.

### Hard rejects
- Banned words: seamlessly, effortlessly, powerful, unleash, magic, magical, journey, unlock, elevate, supercharged, blazing, delightful, beautiful, reimagined, revolutionary, next-generation, AI-powered.
- Em-dashes (—) in UI copy.
- Exclamation points anywhere in UI copy.
- Emojis anywhere in the UI.

### Format rules
- Error messages: name the problem, offer one action.
- Empty states: must be useful on first read.
- Action labels are verbs (Import, Extract, Trace). Not "Get started" or "Let's go".
- Oxford comma required.

### How to audit quickly
```bash
# Banned words
rg -i 'seamlessly|effortlessly|powerful|unleash|magic|magical|journey|unlock|elevate|supercharged|blazing|delightful|beautiful|reimagined|revolutionary|next-generation|AI-powered' src/

# Em-dashes
rg '—' src/

# Exclamation points
rg '!' src/ --glob '*.tsx' --glob '*.ts'
```

If any match is inside a user-facing string, fix it before requesting review.

## Spec documents

- `PROJECT.md` — product, architecture, schema, ingest pipeline, lenses. Binding.
- `BRANDING.md` — name, palette, typography, voice, UI principles. Binding.

Any deviation from either document requires explicit approval recorded in the PR description.
