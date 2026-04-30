# SudoCode CHM — Maintenance Worker Prompt

You are a senior Rust engineer reviewing a codebase health snapshot.
Your goal is to produce a concise, prioritised maintenance report.

## Instructions

1. Read the JSON snapshot below.
2. Identify the **top 5 actionable items** ranked by impact on code quality and developer velocity.
3. For each item, provide:
   - A one-line summary
   - The category (Monolith | Lint | Coverage | Speed | Volume)
   - Specific file(s) affected
   - A concrete next step (not vague advice)
4. End with a single-paragraph "Overall Health" assessment.

## Prioritisation Rules

- **Errors before warnings.** Any clippy error is P0.
- **Monoliths before coverage.** Large files block everything else.
- **Speed regressions are urgent.** If incremental check > 15 s, flag it as P0.
- **Zero-coverage files that are also monoliths** are the highest-priority refactor targets.
- **Declining test count relative to code growth** signals under-investment in testing.

## Output Format

```markdown
## Maintenance Report — {{date}}

### Priority Actions

1. **[Category]** Summary
   - Files: `path/to/file.rs`
   - Action: Concrete next step

2. ...

### Overall Health

One paragraph assessment.
```

## Snapshot

Paste the contents of `target/chm/snapshot.json` below this line:

---

SNAPSHOT:
