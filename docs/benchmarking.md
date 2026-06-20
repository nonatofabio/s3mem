# Benchmarking s3mem

A plan for evaluating s3mem as an agent memory system. Nothing here is built yet — this is
the design of record for when we run a proper benchmark.

## What we're actually measuring

s3mem is a **retriever** (BM25 + grep + a link graph) over a portable store. An agent's
answer quality depends on two separable things:

1. **Retrieval** — given a question, does `recall`/`grep` surface the memories that contain
   the answer?
2. **Reading** — given those memories, does the reader LLM produce a correct answer?

These must be measured **separately**. End-to-end QA accuracy conflates the two and is
dominated by the reader model; it tells you little about whether s3mem itself is good. The
retrieval metric is the actionable one for tuning s3mem (field weights, tokenizer, the cache,
whether to add vector recall) and it needs **no reader LLM**, so it's cheap and repeatable.

> **Headline rule:** lead with retrieval metrics; treat end-to-end QA as a secondary,
> reader-dependent number.

## Metrics

### Retrieval (primary, LLM-free)

For each question the dataset labels the *evidence* (the sessions/messages that contain the
answer). Ingest the history as memories, run `recall(question, k)`, and compute:

| Metric | What it tells you |
|---|---|
| **recall@k** (k = 1, 5, 10, 20) | Is the gold evidence in the top-k? The headline number. |
| **MRR** | How highly is the first relevant memory ranked? |
| **nDCG@k** | Rank-quality when several memories are relevant (multi-hop). |
| **context precision** | Of the k returned, how many are actually relevant (noise the reader must wade through). |

Break every metric down **by the dataset's question type** (temporal, multi-session,
knowledge-update, single-session, abstention). The per-type breakdown is where s3mem's
strengths and weaknesses show up — an average hides them.

### End-to-end QA (secondary, reader-dependent)

Feed the top-k memory bodies to a reader LLM and score the answer (exact-match / F1 where the
dataset supports it, otherwise LLM-as-judge correctness). Report the **reader model and k**
alongside the number — it's not comparable across different readers.

### Operational

- **Recall latency**, cold (cache miss → rebuild) vs warm (cache hit), local vs S3.
- **Index/footprint**: `recall-index.json` size vs corpus size.
- **Cost**: S3 GETs per recall; reader tokens per question.

## Datasets

### LongMemEval — primary

The better **diagnostic** choice. ~500 questions designed to isolate five long-term-memory
abilities, each mapping to a real s3mem concern:

| Ability | Why it stresses s3mem |
|---|---|
| Information extraction | Baseline: can recall find a single stated fact. |
| Multi-session reasoning | One `recall` may not gather evidence spread across sessions — tests whether top-k (or `neighbors`) assembles it. |
| **Temporal reasoning** | s3mem does **not** rank by recency yet — exposes that gap directly. |
| **Knowledge updates** | When a fact changes, does recall surface the *latest* version? We overwrite by id but BM25 ignores `updated`. |
| **Abstention** | When the answer isn't in memory, recall should return nothing — measures confabulation risk. |

It ships in sizes (e.g. a ~115k-token variant and a much larger one) and is built to be
retrieval-augmented, which maps cleanly onto "recall top-k → read". Use it as the primary
benchmark and **always report the per-ability breakdown**.

### LoCoMo — secondary

Very long multi-session conversations (hundreds of turns, tens of sessions) with QA
(single-hop, multi-hop, temporal, adversarial) and event-summarization tasks. Widely cited
(Mem0, Zep, etc.), so it's good for **comparability with published numbers**. Caveats: the
released set is small (~10 conversations) and long-context LLMs can do well on it *without*
explicit memory, so it's a weaker standalone signal of memory quality. Use it for
cross-system comparison, not as the primary driver.

### Others worth a look later

- **HotpotQA / MuSiQue** style multi-hop — to stress the graph (`neighbors`) as a way to
  gather multi-hop evidence lexical recall misses.
- A tiny **hand-authored regression set** (10–20 Q/A over a known bundle) checked into the
  repo, so retrieval quality has a fast, deterministic guard independent of external datasets.

## Mapping a conversational benchmark onto s3mem

The datasets are dialogues, not OKF notes, so ingestion is a small adapter:

- One memory per message (or per session, configurable). `id` = `session-<i>-msg-<j>`;
  `type` = `episodic`; `body` = the message text; `tags` = session id / speaker;
  `created`/`updated` = the message timestamp (the datasets provide these — important for the
  temporal/update buckets).
- For each question: `recall(question, k)` → check whether any returned id is in the
  question's gold-evidence set → that's a hit for recall@k.
- For QA: pass the top-k bodies to the reader.
- Optional: after the top recall hit, expand with `neighbors --depth 1` to test whether the
  link graph helps multi-hop (only meaningful once memories are linked during ingestion).

## What we expect these to expose (s3mem's known gaps)

Being honest up front about where a lexical retriever will hurt — the benchmark should
quantify each:

- **Paraphrase** — BM25 is lexical; questions that paraphrase the memory will miss. This is
  the single biggest expected gap and the main argument for optional vector recall.
- **Recency / updates** — recall ignores `created`/`updated`, so an updated fact and its stale
  prior version score the same. Knowledge-update and temporal buckets will be weak.
- **Multi-hop assembly** — a single ranked list may not collect evidence from several
  sessions; `neighbors` only helps if edges exist.
- **Abstention** — empty recall is the right signal, but the reader must be prompted to use
  it; measure false-answer rate when evidence is absent.

## The decision the first benchmark should answer

> **Is BM25 (+ grep + graph) good enough, or is vector recall worth building?**

Run the retrieval harness with the current lexical recall, then against an embedding baseline
(e.g. recall top-k by cosine over off-the-shelf embeddings) on the same ingested bundles.
Compare **recall@k per question type**. If lexical recall is materially below the embedding
baseline on the paraphrase-heavy buckets (multi-session reasoning, information extraction),
that quantifies the payoff of the roadmap's *optional vector recall* — and tells us whether
it's worth the portability cost of an embedding dependency. If the gap is small, BM25 stays
and we've saved the complexity. Either way the decision is data-driven, not a guess.

## Proposed harness

Keep it deterministic and LLM-free for the retrieval stage:

1. **Loader** — parse the dataset JSON into `(history_messages, questions[], gold_evidence[])`.
2. **Ingest** — write messages into a `LocalStore` bundle via the existing API.
3. **Retrieval eval** — for each question, `recall`/`grep`, score recall@k / MRR / nDCG,
   aggregate per question type. Emit a report (table + JSON).
4. **QA eval (optional, gated)** — top-k bodies → reader LLM → score. Gated on an API key like
   the live S3 test, so the retrieval stage runs anywhere.

Ship it as a separate `bench` binary or `examples/`, not a default build dependency, so the
core crate stays lean. Datasets are downloaded on demand, never vendored.

## Reporting

A run should produce: a per-question-type recall@k table, the overall MRR/nDCG, latency
(cold/warm), and — when the QA stage runs — accuracy with the reader model and k noted. Track
these over time so changes to tokenization, weights, or the cache show up as movement in the
retrieval numbers.
