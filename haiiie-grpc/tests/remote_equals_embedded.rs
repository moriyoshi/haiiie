//! The property that justifies the service boundary: wire operations return
//! exactly what the equivalent in-process operations return.

use std::net::SocketAddr;

use haiiie_core::{DocId, Index, Metric, PathHint};
use haiiie_grpc::{HaiiieClient, HaiiieService, v1};
use haiiie_proto::v1::haiiie_server::HaiiieServer;
use haiiie_testkit::{Corpus, MemStore, Shape};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Channel;

async fn connect(addr: SocketAddr) -> HaiiieClient<Channel> {
    let channel = Channel::from_shared(format!("http://{addr}"))
        .expect("uri")
        .connect()
        .await
        .expect("connect");
    HaiiieClient::new(channel)
}

const METRICS: [(v1::Metric, Metric); 4] = [
    (v1::Metric::Dot, Metric::Dot),
    (v1::Metric::Hamming, Metric::Hamming),
    (v1::Metric::Jaccard, Metric::Jaccard),
    (v1::Metric::Cosine, Metric::Cosine),
];

fn build(corpus: &Corpus) -> Index<MemStore> {
    let index = Index::create(MemStore::new(), 6, corpus.dims).expect("create");
    let mut writer = index.writer();
    for i in 0..corpus.len() {
        writer.put(DocId(i as u64), corpus.code(i)).expect("put");
        if i % 3 == 0 {
            writer.attr(DocId(i as u64), 1).expect("attr");
        }
    }
    writer.commit().expect("commit");
    index.refresh_stats().expect("stats");
    index
}

async fn serve(index: Index<MemStore>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let service = HaiiieService::new(index);
    let handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(HaiiieServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .expect("serve");
    });
    (address, handle)
}

fn dense(corpus: &Corpus, i: usize) -> v1::Code {
    dense_words(corpus.codes[i].clone())
}

fn dense_words(words: Vec<u64>) -> v1::Code {
    v1::Code {
        representation: Some(v1::code::Representation::Dense(v1::DenseCode { words })),
    }
}

fn sparse(positions: Vec<u32>) -> v1::Code {
    v1::Code {
        representation: Some(v1::code::Representation::Sparse(v1::SparseCode {
            positions,
        })),
    }
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

fn search_request(query: v1::Query, limit: u32, filter: Option<v1::Filter>) -> v1::SearchRequest {
    v1::SearchRequest {
        query: Some(query),
        limit,
        filter,
        scan: None,
    }
}

fn explain_request(query: v1::Query, limit: u32) -> v1::ExplainRequest {
    v1::ExplainRequest {
        query: Some(query),
        limit,
        filter: None,
    }
}

fn binary_score(hit: &v1::SearchHit) -> &v1::BinaryScore {
    match hit.score.as_ref().expect("score") {
        v1::search_hit::Score::Binary(score) => score,
        v1::search_hit::Score::Residual(_) => panic!("binary query returned residual score"),
    }
}

fn residual_score(hit: &v1::SearchHit) -> i64 {
    match hit.score.as_ref().expect("score") {
        v1::search_hit::Score::Residual(score) => score.value,
        v1::search_hit::Score::Binary(_) => panic!("residual query returned binary score"),
    }
}

fn binary_document(id: u64, words: &[u64], terms: Vec<u32>) -> v1::Document {
    v1::Document {
        id,
        value: Some(v1::document::Value::BinaryCode(dense_words(words.to_vec()))),
        terms,
    }
}

fn ingest_request(documents: Vec<v1::Document>, delete_ids: Vec<u64>) -> v1::IngestRequest {
    let mut mutations = documents
        .into_iter()
        .map(|document| v1::Mutation {
            operation: Some(v1::mutation::Operation::Put(document)),
        })
        .collect::<Vec<_>>();
    mutations.extend(delete_ids.into_iter().map(|id| v1::Mutation {
        operation: Some(v1::mutation::Operation::Delete(id)),
    }));
    v1::IngestRequest { mutations }
}

#[tokio::test]
async fn a_search_over_the_wire_equals_the_same_search_in_process() {
    let corpus = Corpus::generate(71, 128, 400, Shape::Balanced);
    let embedded = build(&corpus);
    let (address, _server) = serve(build(&corpus)).await;
    let mut client = connect(address).await;

    for (wire_metric, metric) in METRICS {
        for limit in [1u32, 5, 20] {
            let local = embedded
                .search()
                .code(corpus.code(0))
                .metric(metric)
                .k(limit as usize)
                .path(PathHint::Inverted)
                .execute()
                .expect("local");
            let remote = client
                .search(search_request(
                    binary_query(dense(&corpus, 0), wire_metric),
                    limit,
                    None,
                ))
                .await
                .expect("remote")
                .into_inner();

            assert_eq!(remote.hits.len(), local.hits.len());
            for (wire_hit, local_hit) in remote.hits.iter().zip(&local.hits) {
                assert_eq!(wire_hit.id, local_hit.id.get());
                let wire_score = binary_score(wire_hit);
                assert_eq!(wire_score.intersection, local_hit.inter);
                assert_eq!(wire_score.document_weight, local_hit.weight);
                let exact = wire_score.exact.as_ref().expect("exact score");
                let same = match local_hit.score {
                    haiiie_core::Score::Int(value) => {
                        exact.numerator == value && exact.denominator == 1 && !exact.square_root
                    }
                    haiiie_core::Score::Ratio { num, den } => {
                        exact.numerator == num as i64
                            && exact.denominator == den
                            && !exact.square_root
                    }
                    haiiie_core::Score::RatioSq { num, den } => {
                        exact.numerator == num as i64
                            && exact.denominator == den as u64
                            && exact.square_root
                    }
                };
                assert!(same, "{metric:?}: exact score changed on the wire");
            }
            assert!(remote.stats.is_some(), "search omitted scan statistics");
        }
    }
}

#[tokio::test]
async fn filters_and_count_cross_the_wire() {
    let corpus = Corpus::generate(73, 128, 300, Shape::Balanced);
    let embedded = build(&corpus);
    let (address, _server) = serve(build(&corpus)).await;
    let mut client = connect(address).await;
    let filter = v1::Filter {
        kind: Some(v1::filter::Kind::Term(1)),
    };

    let remote = client
        .search(search_request(
            binary_query(dense(&corpus, 2), v1::Metric::Hamming),
            6,
            Some(filter.clone()),
        ))
        .await
        .expect("remote")
        .into_inner();
    let local = embedded
        .search()
        .code(corpus.code(2))
        .metric(Metric::Hamming)
        .k(6)
        .filter(haiiie_core::Filter::Term(1))
        .execute()
        .expect("local");
    assert_eq!(
        remote.hits.iter().map(|hit| hit.id).collect::<Vec<_>>(),
        local
            .hits
            .iter()
            .map(|hit| hit.id.get())
            .collect::<Vec<_>>()
    );

    let count = client
        .count(v1::CountRequest {
            filter: Some(filter),
        })
        .await
        .expect("count")
        .into_inner();
    assert_eq!(count.count, 100);
    let all = client
        .count(v1::CountRequest {
            filter: Some(v1::Filter {
                kind: Some(v1::filter::Kind::All(v1::MatchAll {})),
            }),
        })
        .await
        .expect("count all")
        .into_inner();
    let none = client
        .count(v1::CountRequest {
            filter: Some(v1::Filter {
                kind: Some(v1::filter::Kind::None(v1::MatchNone {})),
            }),
        })
        .await
        .expect("count none")
        .into_inner();
    assert_eq!((all.count, none.count), (300, 0));
}

#[tokio::test]
async fn an_unspecified_metric_is_refused() {
    let corpus = Corpus::generate(79, 64, 50, Shape::Balanced);
    let (address, _server) = serve(build(&corpus)).await;
    let mut client = connect(address).await;
    let error = client
        .search(search_request(
            binary_query(dense(&corpus, 0), v1::Metric::Unspecified),
            5,
            None,
        ))
        .await
        .expect_err("must refuse");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(error.message().contains("unspecified"));

    let mut unknown_consistency =
        search_request(binary_query(dense(&corpus, 0), v1::Metric::Dot), 5, None);
    unknown_consistency.scan = Some(v1::ScanPolicy {
        consistency: 99,
        max_retries: 0,
    });
    let error = client
        .search(unknown_consistency)
        .await
        .expect_err("unknown consistency must be refused");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);

    let error = client
        .search(search_request(
            binary_query(dense(&corpus, 0), v1::Metric::Dot),
            5,
            Some(v1::Filter { kind: None }),
        ))
        .await
        .expect_err("an empty filter must be refused");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);

    let error = client
        .ingest(tokio_stream::iter([v1::IngestRequest {
            mutations: vec![v1::Mutation { operation: None }],
        }]))
        .await
        .expect_err("an empty mutation must be refused");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn ingest_streams_and_describes_the_index_kind() {
    let corpus = Corpus::generate(83, 64, 120, Shape::Balanced);
    let index = Index::create(MemStore::new(), 6, corpus.dims).expect("create");
    let (address, _server) = serve(index).await;
    let mut client = connect(address).await;
    let batches = corpus
        .codes
        .chunks(25)
        .enumerate()
        .map(|(batch, codes)| {
            ingest_request(
                codes
                    .iter()
                    .enumerate()
                    .map(|(offset, code)| {
                        binary_document((batch * 25 + offset) as u64, code, Vec::new())
                    })
                    .collect(),
                Vec::new(),
            )
        })
        .collect::<Vec<_>>();

    let response = client
        .ingest(tokio_stream::iter(batches))
        .await
        .expect("ingest")
        .into_inner();
    assert_eq!(response.documents_written, 120);
    let description = client
        .describe(v1::DescribeRequest {})
        .await
        .expect("describe")
        .into_inner();
    assert_eq!(description.live_documents, 120);
    let Some(v1::describe_response::Index::Binary(binary)) = description.index else {
        panic!("binary index described as residual");
    };
    assert_eq!(binary.code_bits, 64);
}

#[tokio::test]
async fn unsorted_sparse_positions_are_refused() {
    let corpus = Corpus::generate(89, 64, 20, Shape::Balanced);
    let (address, _server) = serve(build(&corpus)).await;
    let mut client = connect(address).await;
    let error = client
        .search(search_request(
            binary_query(sparse(vec![5, 3, 9]), v1::Metric::Dot),
            3,
            None,
        ))
        .await
        .expect_err("must refuse");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
}

async fn explain_agrees(
    embedded: &Index<MemStore>,
    client: &mut HaiiieClient<Channel>,
    corpus: &Corpus,
) {
    for (wire_metric, metric) in METRICS {
        for limit in [1u32, 10] {
            let local = embedded
                .search()
                .code(corpus.code(0))
                .metric(metric)
                .k(limit as usize)
                .explain()
                .expect("local explain");
            let remote = client
                .explain(explain_request(
                    binary_query(dense(corpus, 0), wire_metric),
                    limit,
                ))
                .await
                .expect("remote explain")
                .into_inner();
            let Some(v1::explain_response::Plan::Binary(wire_plan)) = remote.plan else {
                panic!("binary query returned residual plan");
            };
            assert_eq!(remote.path, format!("{:?}", local.path));
            assert_eq!(remote.path_reason, local.path_reason);
            assert_eq!(remote.kernel, format!("{:?}", local.kernel));
            assert_eq!(wire_plan.query_bits, local.query_bits);
            assert_eq!(wire_plan.code_bits, local.dims);
            assert_eq!(
                wire_plan.accumulator_levels as usize,
                local.accumulator_levels
            );
            assert_eq!(remote.blocks, local.blocks);
            assert_eq!(wire_plan.blocks_with_stats, local.blocks_with_stats);
            assert_eq!(remote.exactness, local.exactness);
            assert_eq!(wire_plan.block_weight_spread, local.block_weight_spread);
        }
    }
}

#[tokio::test]
async fn explain_over_the_wire_equals_the_in_process_plan() {
    let corpus = Corpus::generate(19, 128, 600, Shape::Balanced);
    let embedded = build(&corpus);
    let (address, _server) = serve(build(&corpus)).await;
    let mut client = connect(address).await;
    explain_agrees(&embedded, &mut client, &corpus).await;

    // A write invalidates statistics, separating `blocks` from
    // `blocks_with_stats` so crossed assignments cannot pass accidentally.
    let embedded = build(&corpus);
    let served = build(&corpus);
    for index in [&embedded, &served] {
        let mut writer = index.writer();
        writer.put(DocId(0), corpus.code(1)).expect("put");
        writer.commit().expect("commit");
    }
    let (address, _server) = serve(served).await;
    let mut client = connect(address).await;
    explain_agrees(&embedded, &mut client, &corpus).await;
}

#[tokio::test]
async fn explain_refuses_an_unspecified_metric_too() {
    let corpus = Corpus::generate(5, 64, 64, Shape::Balanced);
    let (address, _server) = serve(build(&corpus)).await;
    let mut client = connect(address).await;
    let error = client
        .explain(explain_request(
            binary_query(dense(&corpus, 0), v1::Metric::Unspecified),
            5,
        ))
        .await
        .expect_err("must refuse");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn an_ingest_that_fails_midway_commits_nothing_since_the_last_flush() {
    let index = Index::create(MemStore::new(), 6, 64).expect("create");
    let (address, _server) = serve(index).await;
    let mut client = connect(address).await;
    let good = ingest_request(
        vec![
            binary_document(0, &[0b1011], Vec::new()),
            binary_document(1, &[0b1101], Vec::new()),
        ],
        Vec::new(),
    );
    let bad = ingest_request(
        vec![v1::Document {
            id: 2,
            value: Some(v1::document::Value::BinaryCode(sparse(vec![9, 3]))),
            terms: Vec::new(),
        }],
        Vec::new(),
    );

    let error = client
        .ingest(tokio_stream::iter([good, bad]))
        .await
        .expect_err("malformed document must fail the stream");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    let description = client
        .describe(v1::DescribeRequest {})
        .await
        .expect("describe")
        .into_inner();
    assert_eq!(description.live_documents, 0);
}

#[tokio::test]
async fn an_out_of_range_wire_term_is_refused() {
    let corpus = Corpus::generate(87, 64, 24, Shape::Balanced);
    let (address, _server) = serve(build(&corpus)).await;
    let mut client = connect(address).await;
    let filter = v1::Filter {
        kind: Some(v1::filter::Kind::Term((1 << 24) + 1)),
    };
    let error = client
        .count(v1::CountRequest {
            filter: Some(filter.clone()),
        })
        .await
        .expect_err("out-of-range count term");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    let error = client
        .search(search_request(
            binary_query(dense(&corpus, 0), v1::Metric::Hamming),
            5,
            Some(filter),
        ))
        .await
        .expect_err("out-of-range search term");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn re_sending_an_ingest_changes_nothing() {
    let corpus = Corpus::generate(11, 64, 60, Shape::Balanced);
    let index = Index::create(MemStore::new(), 6, corpus.dims).expect("create");
    let (address, _server) = serve(index).await;
    let mut client = connect(address).await;
    let batch = || {
        ingest_request(
            corpus
                .codes
                .iter()
                .enumerate()
                .map(|(i, code)| binary_document(i as u64, code, Vec::new()))
                .collect(),
            Vec::new(),
        )
    };
    let search = search_request(
        binary_query(dense(&corpus, 0), v1::Metric::Jaccard),
        10,
        None,
    );

    client
        .ingest(tokio_stream::iter([batch()]))
        .await
        .expect("first ingest");
    let first = client
        .search(search.clone())
        .await
        .expect("search")
        .into_inner();
    client
        .ingest(tokio_stream::iter([batch()]))
        .await
        .expect("second ingest");
    let second = client.search(search).await.expect("search").into_inner();
    assert_eq!(first.hits, second.hits);
    let description = client
        .describe(v1::DescribeRequest {})
        .await
        .expect("describe")
        .into_inner();
    assert_eq!(description.live_documents, 60);
}

#[tokio::test]
async fn terms_and_deletes_cross_the_wire() {
    let index = Index::create(MemStore::new(), 6, 64).expect("create");
    let (address, _server) = serve(index).await;
    let mut client = connect(address).await;
    client
        .ingest(tokio_stream::iter([ingest_request(
            vec![
                binary_document(0, &[0b1111], vec![7]),
                binary_document(1, &[0b1110], vec![7, 9]),
                binary_document(2, &[0b1100], Vec::new()),
            ],
            Vec::new(),
        )]))
        .await
        .expect("ingest");

    let by_term = |term| {
        search_request(
            binary_query(dense_words(vec![0b1111]), v1::Metric::Hamming),
            10,
            Some(v1::Filter {
                kind: Some(v1::filter::Kind::Term(term)),
            }),
        )
    };
    let ids =
        |response: v1::SearchResponse| response.hits.iter().map(|hit| hit.id).collect::<Vec<_>>();
    let mut seven = ids(client
        .search(by_term(7))
        .await
        .expect("search")
        .into_inner());
    seven.sort_unstable();
    assert_eq!(seven, vec![0, 1]);
    assert_eq!(
        ids(client
            .search(by_term(9))
            .await
            .expect("search")
            .into_inner()),
        vec![1]
    );

    let response = client
        .ingest(tokio_stream::iter([ingest_request(Vec::new(), vec![1])]))
        .await
        .expect("delete")
        .into_inner();
    assert_eq!(response.documents_deleted, 1);
    assert_eq!(
        ids(client
            .search(by_term(7))
            .await
            .expect("search")
            .into_inner()),
        vec![0]
    );
}

fn residual_model() -> haiiie_embed::ResidualModel {
    let dims = 4usize;
    let mut encoder = vec![0.0; haiiie_embed::RESIDUAL_SIGNS * dims];
    for sign in 0..haiiie_embed::RESIDUAL_SIGNS {
        encoder[sign * dims + sign % dims] = 0.001 + (sign % 7) as f32 * 0.0001;
    }
    let mut decoder = vec![0i16; haiiie_embed::RESIDUAL_SIGNS * dims];
    decoder[0] = 1;
    for sign in 1..haiiie_embed::RESIDUAL_SIGNS {
        decoder[sign * dims] = 2;
    }
    haiiie_embed::ResidualModel::new(dims as u32, 5_000, 1e-7, encoder, decoder)
        .expect("valid residual model")
}

async fn serve_with_residual(
    index: Index<MemStore>,
    model: haiiie_embed::ResidualModel,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let service = HaiiieService::new(index)
        .with_residual_model(model)
        .expect("model matches index");
    let handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(HaiiieServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .expect("serve");
    });
    (address, handle)
}

fn document_embedding(document: &v1::Document) -> &[f32] {
    match document.value.as_ref().expect("document value") {
        v1::document::Value::Embedding(embedding) => &embedding.values,
        v1::document::Value::BinaryCode(_) => panic!("expected embedding"),
    }
}

#[tokio::test]
async fn residual_vectors_ingest_and_search_exactly_over_the_wire() {
    let model = residual_model();
    let index = Index::create_with_model_id(MemStore::new(), 14, 512, model.id().as_bytes())
        .expect("create model-bound index");
    let (address, _server) = serve_with_residual(index, model.clone()).await;
    let mut client = connect(address).await;
    let documents = (0..256usize)
        .map(|id| {
            let values = vec![
                id as f32 + 1.0,
                (id % 5) as f32 - 2.0,
                (id % 7) as f32 * 0.5 + 0.25,
                1.0 - (id % 3) as f32,
            ];
            v1::Document {
                id: id as u64,
                value: Some(v1::document::Value::Embedding(v1::Embedding { values })),
                terms: if id % 3 == 0 { vec![7] } else { Vec::new() },
            }
        })
        .collect::<Vec<_>>();
    client
        .ingest(tokio_stream::iter([ingest_request(
            documents.clone(),
            Vec::new(),
        )]))
        .await
        .expect("residual ingest");

    let query = vec![1.0, -0.25, 0.5, 0.125];
    let prepared = model.prepare_query(&query).expect("prepare");
    let codes = documents
        .iter()
        .map(|document| {
            model
                .encode_document(document_embedding(document))
                .expect("encode oracle")
        })
        .collect::<Vec<_>>();
    let expected = prepared.top_k(
        codes
            .iter()
            .enumerate()
            .filter(|(id, _)| *id % 3 == 0)
            .map(|(id, code)| (DocId(id as u64), code)),
        8,
    );
    let filter = Some(v1::Filter {
        kind: Some(v1::filter::Kind::Term(7)),
    });
    let request = search_request(residual_query(query.clone()), 8, filter.clone());
    let response = client
        .search(request)
        .await
        .expect("residual search")
        .into_inner();
    assert_eq!(
        response.stats.as_ref().expect("stats").documents_scored,
        documents
            .iter()
            .filter(|document| document.id % 3 == 0)
            .count() as u64
    );
    assert_eq!(
        response
            .hits
            .iter()
            .map(|hit| (DocId(hit.id), residual_score(hit)))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|hit| (hit.id, hit.score))
            .collect::<Vec<_>>()
    );

    let description = client
        .describe(v1::DescribeRequest {})
        .await
        .expect("describe")
        .into_inner();
    let Some(v1::describe_response::Index::Residual(residual)) = description.index else {
        panic!("residual index described as binary");
    };
    assert_eq!(residual.model_id, model.id().as_bytes());
    assert_eq!(residual.embedding_dimensions, Some(model.dims()));

    let explained = client
        .explain(v1::ExplainRequest {
            query: Some(residual_query(query)),
            limit: 8,
            filter,
        })
        .await
        .expect("explain")
        .into_inner();
    assert_eq!(explained.kernel, "ResidualLut");
    assert!(matches!(
        explained.plan,
        Some(v1::explain_response::Plan::Residual(_))
    ));

    // The oneofs make an ambiguous query or document unrepresentable. The
    // remaining invalid state is a representation that does not match the
    // index kind, and the service refuses it before writing or scoring.
    let error = client
        .search(search_request(
            binary_query(
                dense(&Corpus::generate(1, 512, 1, Shape::Balanced), 0),
                v1::Metric::Dot,
            ),
            8,
            None,
        ))
        .await
        .expect_err("binary query must not search a residual index");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    let error = client
        .ingest(tokio_stream::iter([ingest_request(
            vec![binary_document(100, &[0; 8], Vec::new())],
            Vec::new(),
        )]))
        .await
        .expect_err("binary document must not enter a residual index");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    let description = client
        .describe(v1::DescribeRequest {})
        .await
        .expect("describe after refusal")
        .into_inner();
    assert_eq!(description.live_documents, documents.len() as u64);
}

#[tokio::test]
async fn residual_ingest_preserves_mutation_order_across_encode_chunks() {
    let model = residual_model();
    let index = Index::create_with_model_id(MemStore::new(), 14, 512, model.id().as_bytes())
        .expect("create model-bound index");
    let (address, _server) = serve_with_residual(index, model.clone()).await;
    let mut client = connect(address).await;
    let first = vec![1.0, 0.0, 0.0, 0.0];
    let last = vec![-1.0, 0.0, 0.0, 0.0];
    let query = last.clone();
    let prepared = model.prepare_query(&query).expect("prepare");
    let first_score = prepared.score(&model.encode_document(&first).expect("first code"));
    let last_score = prepared.score(&model.encode_document(&last).expect("last code"));
    assert_ne!(
        first_score, last_score,
        "test vectors must distinguish order"
    );

    let mut mutations = (0..1023)
        .map(|_| v1::Mutation {
            operation: Some(v1::mutation::Operation::Delete(99)),
        })
        .collect::<Vec<_>>();
    let put = |values: Vec<f32>| v1::Mutation {
        operation: Some(v1::mutation::Operation::Put(v1::Document {
            id: 7,
            value: Some(v1::document::Value::Embedding(v1::Embedding { values })),
            terms: vec![12],
        })),
    };
    mutations.push(put(first));
    mutations.push(v1::Mutation {
        operation: Some(v1::mutation::Operation::Delete(7)),
    });
    mutations.push(put(last));
    let response = client
        .ingest(tokio_stream::iter([v1::IngestRequest { mutations }]))
        .await
        .expect("ingest across chunk boundary")
        .into_inner();
    assert_eq!(response.documents_written, 2);
    assert_eq!(response.documents_deleted, 1024);
    let result = client
        .search(search_request(residual_query(query), 1, None))
        .await
        .expect("search final row")
        .into_inner();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].id, 7);
    assert_eq!(residual_score(&result.hits[0]), last_score);
}

#[tokio::test]
async fn residual_ingest_preserves_order_after_a_packed_tile_across_messages() {
    let model = residual_model();
    let index = Index::create_with_model_id(MemStore::new(), 17, 512, model.id().as_bytes())
        .expect("create model-bound index");
    let (address, _server) = serve_with_residual(index, model.clone()).await;
    let mut client = connect(address).await;
    let first = vec![1.0, 0.0, 0.0, 0.0];
    let last = vec![-1.0, 0.0, 0.0, 0.0];
    let prepared = model.prepare_query(&last).expect("prepare");
    let expected = prepared.score(&model.encode_document(&last).expect("last code"));
    let put = |id, values: Vec<f32>| v1::Mutation {
        operation: Some(v1::mutation::Operation::Put(v1::Document {
            id,
            value: Some(v1::document::Value::Embedding(v1::Embedding { values })),
            terms: Vec::new(),
        })),
    };
    let first_message = v1::IngestRequest {
        mutations: (0..1024).map(|id| put(id, first.clone())).collect(),
    };
    let second_message = v1::IngestRequest {
        mutations: vec![
            v1::Mutation {
                operation: Some(v1::mutation::Operation::Delete(0)),
            },
            put(0, last.clone()),
        ],
    };
    let response = client
        .ingest(tokio_stream::iter([first_message, second_message]))
        .await
        .expect("ingest both messages")
        .into_inner();
    assert_eq!(response.documents_written, 1025);
    assert_eq!(response.documents_deleted, 1);
    let result = client
        .search(search_request(residual_query(last), 1, None))
        .await
        .expect("search final row")
        .into_inner();
    assert_eq!(result.hits[0].id, 0);
    assert_eq!(residual_score(&result.hits[0]), expected);
}

#[tokio::test]
async fn residual_ingest_failure_keeps_only_the_committed_packed_prefix() {
    let model = residual_model();
    let index = Index::create_with_model_id(MemStore::new(), 18, 512, model.id().as_bytes())
        .expect("create model-bound index");
    let (address, _server) = serve_with_residual(index, model).await;
    let mut client = connect(address).await;
    let tile = v1::IngestRequest {
        mutations: (0..1024)
            .map(|id| v1::Mutation {
                operation: Some(v1::mutation::Operation::Put(v1::Document {
                    id,
                    value: Some(v1::document::Value::Embedding(v1::Embedding {
                        values: vec![1.0, 0.0, 0.0, 0.0],
                    })),
                    terms: Vec::new(),
                })),
            })
            .collect(),
    };
    let invalid = ingest_request(vec![binary_document(1024, &[0; 8], Vec::new())], Vec::new());
    let error = client
        .ingest(tokio_stream::iter([tile, invalid]))
        .await
        .expect_err("binary value must fail residual ingest");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    let description = client
        .describe(v1::DescribeRequest {})
        .await
        .expect("describe committed prefix")
        .into_inner();
    assert_eq!(description.live_documents, 1024);
}

#[tokio::test]
async fn a_model_bound_service_refuses_search_without_its_model() {
    let model = residual_model();
    let index = Index::create_with_model_id(MemStore::new(), 15, 512, model.id().as_bytes())
        .expect("create model-bound index");
    let (address, _server) = serve(index).await;
    let mut client = connect(address).await;
    let error = client
        .search(search_request(
            residual_query(vec![1.0, 0.0, 0.0, 0.0]),
            10,
            None,
        ))
        .await
        .expect_err("missing model must be refused");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    let wrong =
        Index::create_with_model_id(MemStore::new(), 16, 512, [0x55; 32]).expect("foreign index");
    assert!(
        HaiiieService::new(wrong)
            .with_residual_model(model)
            .is_err(),
        "service accepted a model whose identity does not match its index"
    );
}
