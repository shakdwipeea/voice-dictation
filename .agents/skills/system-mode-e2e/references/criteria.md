# System Mode E2E criteria

| Area | Required evidence |
| --- | --- |
| Static | Workspace build/tests, clippy with warnings denied, scoped formatting, phase 0/1/2 and UI Python suites |
| Routing | Application, URL, browser+URL, web search, file, folder, project, project+editor, and adversarial rejection |
| Target safety | Approved roots, canonicalization, symlink escape, hidden/unsafe types, mutation, expiry, stale/replay, cancellation, deadlines, and caps |
| Palette | All target kinds, deterministic rank, ambiguity retained, no-match, explicit one-time selection |
| Daemon | Mock ASR plus headless overlay drives System press/release through worker discovery and palette cancellation; normal dictation tests remain green |
| Live macOS | Explicit opt-in only: harmless URL, disposable file/folder open and reveal, real project in resolved editor |
| Live Linux | Explicit opt-in only on Linux: GIO application/URL and disposable local targets; otherwise SKIPPED |
| Reporting | JSON artifact, Markdown/HTML report, redacted logs, pass/fail/skip counts, host, commands, observed live actions, and cleanup |

Non-live thresholds: fixture top-1/top-5 recall 100%, false actions zero,
small-fixture search under 500ms, cancellation under 50ms, and no real native
target action. Platform live checks report native acceptance/typed evidence and
must never be inferred from a stub or a different OS.
