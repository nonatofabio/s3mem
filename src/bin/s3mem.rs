//! `s3mem` — CLI over an OKF memory bundle. The agent-facing surface the skill wraps.
//!
//! Backend + namespace come from flags or env (`S3MEM_PATH` for local, `S3MEM_BUCKET`
//! [+ `S3MEM_PREFIX`] for S3, `S3MEM_NAMESPACE`). The two recall tools (`recall`, `grep`)
//! print JSON by default for easy parsing; `--pretty` switches to human output.

use std::io::Read;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use s3mem::{Filter, GrepOptions, LocalStore, MemoryType, Record, RecordMeta, Store};

#[derive(Parser)]
#[command(
    name = "s3mem",
    about = "Store and recall OKF agent memories over a filesystem or S3"
)]
struct Cli {
    /// Local bundle root (local backend). Mutually exclusive with --bucket.
    #[arg(long, env = "S3MEM_PATH", global = true)]
    path: Option<PathBuf>,
    /// S3 bucket (S3 backend; requires a binary built with --features s3).
    #[arg(long, env = "S3MEM_BUCKET", global = true)]
    bucket: Option<String>,
    /// Optional key prefix under the bucket.
    #[arg(long, env = "S3MEM_PREFIX", default_value = "", global = true)]
    prefix: String,
    /// Bundle namespace (per-agent / per-project).
    #[arg(
        long,
        env = "S3MEM_NAMESPACE",
        default_value = "default",
        global = true
    )]
    namespace: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Recall memories by relevance (BM25 ranked). Use for fuzzy, natural-language lookups.
    Recall {
        query: String,
        /// Max results.
        #[arg(long, default_value_t = 10)]
        k: usize,
        /// Restrict to these memory types (repeatable).
        #[arg(long = "type")]
        kinds: Vec<MemoryType>,
        /// Require these tags (repeatable, AND).
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Human-readable output instead of JSON.
        #[arg(long)]
        pretty: bool,
    },
    /// Search memories by pattern (grep). Use for exact tokens, identifiers, or regex.
    Grep {
        pattern: String,
        /// Treat the pattern as a regex (default: literal substring).
        #[arg(long)]
        regex: bool,
        /// Case-sensitive matching (default: insensitive).
        #[arg(short = 's', long = "case-sensitive")]
        case_sensitive: bool,
        #[arg(long = "type")]
        kinds: Vec<MemoryType>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        pretty: bool,
    },
    /// Print a memory's full markdown by id.
    Get { id: String },
    /// List all memory ids.
    List,
    /// Write (create or overwrite) a memory; body comes from --body or stdin.
    Remember {
        #[arg(long)]
        id: String,
        #[arg(long = "type", default_value = "semantic")]
        kind: MemoryType,
        #[arg(long)]
        description: String,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        source: Option<String>,
        /// Body text; if omitted, read from stdin.
        #[arg(long)]
        body: Option<String>,
    },
    /// Delete a memory by id.
    Forget { id: String },
    /// Create a mutual link between two memories.
    Link { a: String, b: String },
    /// Remove the mutual link between two memories.
    Unlink { a: String, b: String },
    /// Show a memory's direct links (frontmatter `links` + body `[[id]]`).
    Links {
        id: String,
        #[arg(long)]
        pretty: bool,
    },
    /// Traverse the link graph outward from a memory.
    Neighbors {
        id: String,
        /// How many hops to follow.
        #[arg(long, default_value_t = 2)]
        depth: usize,
        #[arg(long)]
        pretty: bool,
    },
}

type CliResult = Result<(), Box<dyn std::error::Error>>;

fn main() {
    if let Err(e) = run() {
        eprintln!("s3mem: {e}");
        std::process::exit(1);
    }
}

fn run() -> CliResult {
    let cli = Cli::parse();
    let store = open_store(&cli)?;

    match cli.command {
        Command::Recall {
            query,
            k,
            kinds,
            tags,
            pretty,
        } => {
            let index = s3mem::load_or_build_index(store.as_ref())?;
            let filter = Filter { kinds, tags };
            let hits = index.search(&query, &filter, k);
            print_hits(&hits, pretty)?;
        }
        Command::Grep {
            pattern,
            regex,
            case_sensitive,
            kinds,
            tags,
            pretty,
        } => {
            let index = s3mem::load_or_build_index(store.as_ref())?;
            let opts = GrepOptions {
                pattern,
                regex,
                case_sensitive,
                filter: Filter { kinds, tags },
                max_snippets: 5,
            };
            let hits = index.grep(&opts)?;
            print_hits(&hits, pretty)?;
        }
        Command::Get { id } => {
            print!("{}", store.get(&id)?.to_markdown()?);
        }
        Command::List => {
            for id in store.list()? {
                println!("{id}");
            }
        }
        Command::Remember {
            id,
            kind,
            description,
            tags,
            source,
            body,
        } => {
            let body = match body {
                Some(b) => b,
                None => read_stdin()?,
            };
            let mut meta = RecordMeta::new(id, kind, description, s3mem::now_iso());
            meta.tags = tags;
            meta.source = source;
            let record = Record::new(meta, body);
            store.put(&record)?;
            eprintln!("remembered `{}`", record.meta.id);
        }
        Command::Forget { id } => {
            store.delete(&id)?;
            eprintln!("forgot `{id}`");
        }
        Command::Link { a, b } => {
            s3mem::link(store.as_ref(), &a, &b)?;
            eprintln!("linked `{a}` <-> `{b}`");
        }
        Command::Unlink { a, b } => {
            s3mem::unlink(store.as_ref(), &a, &b)?;
            eprintln!("unlinked `{a}` <-> `{b}`");
        }
        Command::Links { id, pretty } => {
            store.get(&id)?; // validate the memory exists
            let records = store.records()?;
            let direct = s3mem::neighbors(&records, &id, 1);
            print_neighbors(&direct, pretty)?;
        }
        Command::Neighbors { id, depth, pretty } => {
            store.get(&id)?;
            let records = store.records()?;
            let reached = s3mem::neighbors(&records, &id, depth);
            print_neighbors(&reached, pretty)?;
        }
    }
    Ok(())
}

fn open_store(cli: &Cli) -> Result<Box<dyn Store>, Box<dyn std::error::Error>> {
    if let Some(bucket) = &cli.bucket {
        return Ok(Box::new(s3mem::S3Store::with_prefix(
            bucket.clone(),
            cli.prefix.clone(),
            cli.namespace.clone(),
        )?));
    }
    if let Some(path) = &cli.path {
        return Ok(Box::new(LocalStore::new(
            path.clone(),
            cli.namespace.clone(),
        )));
    }
    Err("no backend selected: set --path/$S3MEM_PATH (local) or --bucket/$S3MEM_BUCKET (s3)".into())
}

fn print_neighbors(neighbors: &[s3mem::Neighbor], pretty: bool) -> CliResult {
    if !pretty {
        println!("{}", serde_json::to_string_pretty(neighbors)?);
        return Ok(());
    }
    if neighbors.is_empty() {
        println!("(no links)");
        return Ok(());
    }
    for n in neighbors {
        let desc = match &n.description {
            Some(d) => d.as_str(),
            None => "(missing)",
        };
        println!("● {} (depth {}) — {desc}", n.id, n.depth);
    }
    Ok(())
}

fn print_hits(hits: &[s3mem::Hit], pretty: bool) -> CliResult {
    if !pretty {
        println!("{}", serde_json::to_string_pretty(hits)?);
        return Ok(());
    }
    if hits.is_empty() {
        println!("(no matches)");
        return Ok(());
    }
    for h in hits {
        match h.score {
            Some(s) => println!("● {} [{}]  (score {s:.3})", h.id, h.kind),
            None => println!("● {} [{}]", h.id, h.kind),
        }
        println!("  {}", h.description);
        for snippet in &h.snippets {
            println!("    {snippet}");
        }
    }
    Ok(())
}

fn read_stdin() -> std::io::Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}
