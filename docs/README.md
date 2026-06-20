# s3mem docs

Design and planning docs that don't belong in the top-level README or CLAUDE.md.

- [**benchmarking.md**](benchmarking.md) — how to evaluate s3mem as an agent memory system:
  which datasets (LongMemEval primary, LoCoMo secondary), what to measure (retrieval metrics
  first, QA second), how to map a conversational benchmark onto the store, and the one
  decision the first benchmark should settle (is BM25 enough, or is vector recall worth it).

For build commands, the source map, and the invariants worth preserving, see
[`../CLAUDE.md`](../CLAUDE.md). For what s3mem is and how to use it, see
[`../README.md`](../README.md).
