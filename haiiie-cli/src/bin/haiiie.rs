//! `haiiie` -- talk to a `haiiied`.

use clap::{Parser, Subcommand};
use haiiie_grpc::{HaiiieClient, v1};
use tonic::transport::Channel;

#[derive(Parser, Debug)]
#[command(name = "haiiie", about = "Query a haiiie index")]
struct Args {
    /// Server address.
    #[arg(long, default_value = "http://127.0.0.1:50071")]
    server: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Index geometry and liveness.
    Describe,
    /// Exact top-k for a code given as hex words.
    Search {
        /// Query code, hex words separated by commas, least significant first.
        code: String,
        #[arg(long, default_value = "hamming")]
        metric: String,
        #[arg(long, default_value_t = 10)]
        k: u32,
        /// Restrict to documents carrying this attribute.
        #[arg(long)]
        term: Option<u32>,
    },
    /// Exact residual-model top-k for a comma-separated float embedding.
    SearchVector {
        vector: String,
        #[arg(long, default_value_t = 10)]
        k: u32,
        #[arg(long)]
        term: Option<u32>,
    },
    /// What a binary search would do, without scoring anything.
    Explain {
        code: String,
        #[arg(long, default_value = "hamming")]
        metric: String,
        #[arg(long, default_value_t = 10)]
        k: u32,
    },
    /// How many documents a filter admits.
    Count {
        #[arg(long)]
        term: Option<u32>,
    },
}

fn parse_code(input: &str) -> Result<v1::Code, String> {
    let words = input
        .split(',')
        .map(|word| u64::from_str_radix(word.trim().trim_start_matches("0x"), 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("bad hex word: {error}"))?;
    Ok(v1::Code {
        representation: Some(v1::code::Representation::Dense(v1::DenseCode { words })),
    })
}

fn parse_vector(input: &str) -> Result<Vec<f32>, String> {
    input
        .split(',')
        .map(|value| {
            value
                .trim()
                .parse()
                .map_err(|error| format!("bad float component: {error}"))
        })
        .collect()
}

fn parse_metric(input: &str) -> Result<v1::Metric, String> {
    Ok(match input.to_ascii_lowercase().as_str() {
        "dot" => v1::Metric::Dot,
        "hamming" => v1::Metric::Hamming,
        "jaccard" => v1::Metric::Jaccard,
        "cosine" => v1::Metric::Cosine,
        other => return Err(format!("unknown metric `{other}`")),
    })
}

fn filter_of(term: Option<u32>) -> Option<v1::Filter> {
    term.map(|term| v1::Filter {
        kind: Some(v1::filter::Kind::Term(term)),
    })
}

fn binary_query(code: v1::Code, metric: v1::Metric) -> v1::Query {
    v1::Query {
        kind: Some(v1::query::Kind::Binary(v1::BinaryQuery {
            code: Some(code),
            metric: metric as i32,
        })),
    }
}

fn residual_query(values: Vec<f32>) -> v1::Query {
    v1::Query {
        kind: Some(v1::query::Kind::Residual(v1::ResidualQuery {
            embedding: Some(v1::Embedding { values }),
        })),
    }
}

fn print_stats(response: &v1::SearchResponse) {
    let stats = response
        .stats
        .as_ref()
        .expect("server returned search stats");
    let resumed = if stats.resumes == 0 {
        String::new()
    } else {
        let versions = stats
            .versions
            .as_ref()
            .expect("server returned version range");
        format!(
            ", {} resumes spanning versions {}..{}",
            stats.resumes, versions.minimum, versions.maximum
        )
    };
    eprintln!(
        "scored {} documents, {} blocks visited, {} skipped{}",
        stats.documents_scored, stats.blocks_visited, stats.blocks_skipped, resumed
    );
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let channel = Channel::from_shared(args.server)?.connect().await?;
    let mut client = HaiiieClient::new(channel);

    match args.command {
        Command::Describe => {
            let description = client.describe(v1::DescribeRequest {}).await?.into_inner();
            match description.index.expect("server returned index kind") {
                v1::describe_response::Index::Binary(binary) => {
                    println!("kind              binary");
                    println!("code bits         {}", binary.code_bits);
                }
                v1::describe_response::Index::Residual(residual) => {
                    println!("kind              residual");
                    println!("code bits         {}", residual.code_bits);
                    match residual.embedding_dimensions {
                        Some(dimensions) => println!("embedding dims    {dimensions}"),
                        None => println!("embedding dims    model not loaded"),
                    }
                }
            }
            println!("live documents    {}", description.live_documents);
            println!("blocks            {}", description.blocks);
            println!("blocks with stats {}", description.blocks_with_stats);
        }
        Command::Search {
            code,
            metric,
            k,
            term,
        } => {
            let response = client
                .search(v1::SearchRequest {
                    query: Some(binary_query(parse_code(&code)?, parse_metric(&metric)?)),
                    limit: k,
                    filter: filter_of(term),
                    scan: None,
                })
                .await?
                .into_inner();
            for (rank, hit) in response.hits.iter().enumerate() {
                let v1::search_hit::Score::Binary(score) =
                    hit.score.as_ref().expect("server returned score")
                else {
                    return Err("server returned a residual score for a binary query".into());
                };
                let exact = score.exact.as_ref().expect("server returned exact score");
                println!(
                    "{:>3}  id={:<12} score={:<12.6} exact={}{}/{}  a={} w={}",
                    rank + 1,
                    hit.id,
                    score.approximate,
                    if exact.square_root { "sqrt " } else { "" },
                    exact.numerator,
                    exact.denominator,
                    score.intersection,
                    score.document_weight
                );
            }
            print_stats(&response);
        }
        Command::SearchVector { vector, k, term } => {
            let response = client
                .search(v1::SearchRequest {
                    query: Some(residual_query(parse_vector(&vector)?)),
                    limit: k,
                    filter: filter_of(term),
                    scan: None,
                })
                .await?
                .into_inner();
            for (rank, hit) in response.hits.iter().enumerate() {
                let v1::search_hit::Score::Residual(score) =
                    hit.score.as_ref().expect("server returned score")
                else {
                    return Err("server returned a binary score for a residual query".into());
                };
                println!("{:>3}  id={:<12} exact={}", rank + 1, hit.id, score.value);
            }
            print_stats(&response);
        }
        Command::Explain { code, metric, k } => {
            let response = client
                .explain(v1::ExplainRequest {
                    query: Some(binary_query(parse_code(&code)?, parse_metric(&metric)?)),
                    limit: k,
                    filter: None,
                })
                .await?
                .into_inner();
            let Some(v1::explain_response::Plan::Binary(plan)) = response.plan else {
                return Err("server returned a residual plan for a binary query".into());
            };
            println!("path       {}  ({})", response.path, response.path_reason);
            println!("kernel     {}", response.kernel);
            println!("accum      {} planes", plan.accumulator_levels);
            println!("query      {} of {} bits", plan.query_bits, plan.code_bits);
            println!(
                "blocks     {} live, {} with statistics",
                response.blocks, plan.blocks_with_stats
            );
            println!("exactness  {}", response.exactness);
        }
        Command::Count { term } => {
            let response = client
                .count(v1::CountRequest {
                    filter: filter_of(term),
                })
                .await?
                .into_inner();
            println!("{}", response.count);
        }
    }
    Ok(())
}
