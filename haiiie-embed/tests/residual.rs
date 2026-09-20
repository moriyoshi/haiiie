//! Fixed residual-code layout, model, scoring and persisted-index properties.

use haiiie_core::{
    CodeRef, DocId, Error, Filter, Index, SetSnapshot, SetStore, YesnoStore,
    acknowledge_compaction, compact,
};
use haiiie_embed::{CodecError, MAX_NORM_CODE, ModelId, PackedCode, RESIDUAL_SIGNS, ResidualModel};
use haiiie_testkit::MemStore;

fn small_residual_model() -> ResidualModel {
    let dims = 4usize;
    let mut encoder = vec![0.0; RESIDUAL_SIGNS * dims];
    for sign in 0..RESIDUAL_SIGNS {
        encoder[sign * dims + sign % dims] = 0.001 + (sign % 7) as f32 * 0.0001;
    }
    // The first decoded component is always odd and therefore nonzero: one
    // coefficient is 1 and the other 499 are 2. This keeps every possible sign
    // assignment valid while making the hand-written oracle simple.
    let mut decoder = vec![0i16; RESIDUAL_SIGNS * dims];
    decoder[0] = 1;
    for sign in 1..RESIDUAL_SIGNS {
        decoder[sign * dims] = 2;
    }
    ResidualModel::new(dims as u32, 5_000, 1e-7, encoder, decoder).expect("valid model")
}

#[test]
fn contiguous_norm_reconstruction_matches_strided_integer_oracle() {
    let dims = 16usize;
    let mut encoder = vec![0.0f32; RESIDUAL_SIGNS * dims];
    let mut decoder = vec![0i16; RESIDUAL_SIGNS * dims];
    for sign in 0..RESIDUAL_SIGNS {
        for dimension in 0..dims {
            encoder[sign * dims + dimension] =
                ((sign * 11 + dimension * 7) % 17) as f32 * 0.0001 - 0.0008;
            decoder[sign * dims + dimension] = ((sign * 13 + dimension * 19) % 41) as i16 - 20;
        }
        // An odd first component rules out a zero decoded vector for every
        // possible sign assignment.
        decoder[sign * dims] = if sign == 0 { 1 } else { 2 };
    }
    let model = ResidualModel::new(dims as u32, 1, 1e-6, encoder, decoder.clone())
        .expect("dense asymmetric decoder");
    for document in 0..32usize {
        let values: Vec<f32> = (0..dims)
            .map(|dimension| ((document * 23 + dimension * 31) % 67) as f32 * 0.01 - 0.33)
            .collect();
        let code = model.encode_document(&values).expect("encode");
        let squared_norm: i64 = (0..dims)
            .map(|dimension| {
                let component: i64 = (0..RESIDUAL_SIGNS)
                    .map(|sign| {
                        let value = i64::from(decoder[sign * dims + dimension]);
                        if code.sign(sign).expect("sign") {
                            value
                        } else {
                            -value
                        }
                    })
                    .sum();
                component * component
            })
            .sum();
        let raw = (1.0 / (squared_norm as f64).sqrt() / 1e-6).round_ties_even() - 1.0;
        let want = raw.clamp(0.0, f64::from(MAX_NORM_CODE)) as u16;
        assert_eq!(code.norm_code(), want, "document {document}");
    }
}

#[test]
fn ordered_parallel_batch_encodes_identically_and_survives_reopen() {
    let model = small_residual_model();
    let documents: Vec<Vec<f32>> = (0..130)
        .map(|id| {
            (0..4)
                .map(|component| ((id * 17 + component * 31) % 53) as f32 * 0.02 - 0.5)
                .collect()
        })
        .collect();
    let vectors: Vec<&[f32]> = documents.iter().map(Vec::as_slice).collect();
    let serial: Vec<_> = vectors
        .iter()
        .map(|vector| model.encode_document(vector).expect("serial encode"))
        .collect();
    for workers in [0, 1, 2, 3, 20, 200] {
        assert_eq!(
            model
                .encode_documents(&vectors, workers)
                .expect("batch encode"),
            serial,
            "worker count {workers}"
        );
    }
    assert!(
        model
            .encode_documents(&[], 20)
            .expect("empty batch")
            .is_empty()
    );
    let mut invalid = vectors.clone();
    invalid[11] = &[];
    invalid[97] = &[];
    assert!(matches!(
        model.encode_documents(&invalid, 20),
        Err(CodecError::DimensionMismatch { want: 4, got: 0 })
    ));

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db");
    let index = Index::create_with_model_id(
        YesnoStore::open(&path).expect("store"),
        14,
        512,
        model.id().as_bytes(),
    )
    .expect("create index");
    {
        let mut writer = index.writer();
        for (id, code) in serial.iter().enumerate() {
            writer
                .put(DocId(id as u64), CodeRef::Dense(&code.to_words()))
                .expect("ordered put");
        }
        writer.commit().expect("commit");
    }
    index.store().flush().expect("checkpoint");
    drop(index);
    let reopened = Index::open(YesnoStore::open(&path).expect("reopen"), 14).expect("index");
    let prepared = model
        .prepare_query(&[0.5, -0.25, 1.0, 0.125])
        .expect("query");
    let want = prepared.top_k(
        serial
            .iter()
            .enumerate()
            .map(|(id, code)| (DocId(id as u64), code)),
        serial.len(),
    );
    let got = prepared
        .search(&reopened)
        .expect("matching index")
        .k(serial.len())
        .execute()
        .expect("search");
    assert_eq!(
        got.hits
            .iter()
            .map(|hit| haiiie_embed::ResidualHit {
                id: hit.id,
                score: hit.score,
            })
            .collect::<Vec<_>>(),
        want
    );
}

#[test]
fn residual_code_uses_the_frozen_500_plus_12_layout() {
    let signs: Vec<bool> = (0..RESIDUAL_SIGNS)
        .map(|bit| bit % 7 == 0 || bit == 499)
        .collect();
    let code = PackedCode::new(&signs, 0xabc).expect("pack");

    assert_eq!(code.as_bytes().len(), 64);
    for (bit, &want) in signs.iter().enumerate() {
        assert_eq!(code.sign(bit), Some(want), "sign {bit}");
    }
    assert_eq!(code.sign(500), None);
    assert_eq!(code.norm_code(), 0xabc);
    assert_eq!(
        PackedCode::from_words(&code.to_words()).expect("word round trip"),
        code
    );
    assert!(PackedCode::from_words(&code.to_words()[..7]).is_err());

    let tail_signs = signs[496..500]
        .iter()
        .enumerate()
        .fold(0u16, |word, (bit, &set)| word | (u16::from(set) << bit));
    assert_eq!(
        &code.as_bytes()[62..64],
        &((0xabcu16 << 4) | tail_signs).to_le_bytes()
    );
    assert!(matches!(
        PackedCode::new(&signs[..499], 0),
        Err(CodecError::DimensionMismatch { .. })
    ));
    assert!(matches!(
        PackedCode::new(&signs, MAX_NORM_CODE + 1),
        Err(CodecError::FieldOutOfRange(_))
    ));
}

#[test]
fn packed_residual_scoring_matches_the_slow_integer_oracle() {
    let model = small_residual_model();
    let prepared = model
        .prepare_query(&[1.0, -0.25, 0.5, 0.125])
        .expect("prepare query");
    let mut codes = Vec::new();
    for document in 0..37usize {
        let signs: Vec<bool> = (0..RESIDUAL_SIGNS)
            .map(|bit| (bit * 17 + document * 13) % 29 < 14)
            .collect();
        codes.push(
            PackedCode::new(&signs, ((document * 311) as u16) & MAX_NORM_CODE).expect("pack"),
        );
    }
    // A byte-identical pair makes the ascending-ID tie rule observable.
    codes[36] = codes[5];

    let slow_score = |code: &PackedCode| {
        let dot: i64 = prepared
            .weights()
            .iter()
            .enumerate()
            .map(|(bit, &weight)| {
                if code.sign(bit).expect("500 signs") {
                    weight
                } else {
                    -weight
                }
            })
            .sum();
        dot * (model.norm_offset() + i64::from(code.norm_code()))
    };
    for code in &codes {
        assert_eq!(prepared.score(code), slow_score(code));
    }

    let mut oracle: Vec<_> = codes
        .iter()
        .enumerate()
        .map(|(id, code)| haiiie_embed::ResidualHit {
            id: DocId(id as u64),
            score: slow_score(code),
        })
        .collect();
    oracle.sort_by(|left, right| right.score.cmp(&left.score).then(left.id.cmp(&right.id)));
    let got = prepared.top_k(
        codes
            .iter()
            .enumerate()
            .map(|(id, code)| (DocId(id as u64), code)),
        12,
    );
    assert_eq!(got, oracle[..12]);
    let full = prepared.top_k(
        codes
            .iter()
            .enumerate()
            .map(|(id, code)| (DocId(id as u64), code)),
        codes.len(),
    );
    assert!(
        full.iter().position(|hit| hit.id == DocId(5))
            < full.iter().position(|hit| hit.id == DocId(36)),
        "equal packed rows did not break their tie by ascending id"
    );
    assert!(
        prepared
            .top_k(codes.iter().map(|code| (DocId(0), code)), 0)
            .is_empty()
    );
}

#[test]
fn a_residual_model_round_trips_with_its_identity_and_bounds() {
    let model = small_residual_model();
    let bytes = model.to_bytes();
    let reopened = ResidualModel::from_bytes(&bytes).expect("reopen model");
    assert_eq!(reopened.id(), model.id());
    assert_eq!(reopened.dims(), model.dims());
    assert_eq!(reopened.norm_offset(), model.norm_offset());
    assert_eq!(reopened.max_abs_score(), model.max_abs_score());
    assert_eq!(
        reopened.max_decoded_squared_norm(),
        model.max_decoded_squared_norm()
    );

    let document = [0.75, -0.5, 0.125, 0.25];
    let query = [-0.25, 1.0, 0.5, -0.125];
    let code = model.encode_document(&document).expect("encode document");
    assert_eq!(
        reopened.encode_document(&document).expect("encode again"),
        code
    );
    assert_eq!(
        reopened
            .prepare_query(&query)
            .expect("prepare")
            .score(&code),
        model.prepare_query(&query).expect("prepare").score(&code)
    );
    reopened.require_id(model.id()).expect("matching identity");
    assert!(
        reopened
            .require_id(ModelId::from_bytes([0x6b; 32]))
            .is_err()
    );

    assert!(ResidualModel::from_bytes(&bytes[..bytes.len() - 1]).is_err());
    let mut foreign = bytes.clone();
    foreign[0] ^= 0xff;
    assert!(ResidualModel::from_bytes(&foreign).is_err());
    let mut damaged = bytes.clone();
    let last = damaged.len() - 1;
    damaged[last] ^= 1;
    assert!(matches!(
        ResidualModel::from_bytes(&damaged),
        Err(CodecError::CorruptModel(_))
    ));
    let mut trailing = bytes;
    trailing.push(0);
    assert!(ResidualModel::from_bytes(&trailing).is_err());
}

#[test]
fn residual_model_validation_rejects_unbounded_or_invalid_math() {
    let dims = 1u32;
    let encoder = vec![1.0; RESIDUAL_SIGNS];
    let decoder = vec![2_047; RESIDUAL_SIGNS];
    assert!(matches!(
        ResidualModel::new(
            dims,
            1_000_000_000_000,
            1e-9,
            encoder.clone(),
            decoder.clone()
        ),
        Err(CodecError::ScoreOverflow)
    ));
    assert!(matches!(
        ResidualModel::new(dims, 1, f64::NAN, encoder.clone(), decoder.clone()),
        Err(CodecError::InvalidModel(_))
    ));
    let mut wide = decoder;
    wide[0] = i16::MAX;
    assert!(matches!(
        ResidualModel::new(dims, 1, 1e-9, encoder, wide),
        Err(CodecError::InvalidModel(_))
    ));
}

#[test]
fn a_residual_model_requires_a_matching_model_bound_index() {
    let model = small_residual_model();
    let bound = Index::create_with_model_id(MemStore::new(), 3, 512, model.id().as_bytes())
        .expect("create model-bound index");
    model
        .require_index_meta(bound.meta())
        .expect("matching model");

    let unbound = Index::create(MemStore::new(), 4, 512).expect("create unbound index");
    assert!(matches!(
        model.require_index_meta(unbound.meta()),
        Err(CodecError::IndexHasNoModelIdentity)
    ));
    let wrong = Index::create_with_model_id(MemStore::new(), 5, 512, [0x77; 32])
        .expect("create differently bound index");
    assert!(matches!(
        model.require_index_meta(wrong.meta()),
        Err(CodecError::ModelMismatch { .. })
    ));
    let wrong_width = Index::create_with_model_id(MemStore::new(), 6, 256, model.id().as_bytes())
        .expect("create narrow index");
    assert!(matches!(
        model.require_index_meta(wrong_width.meta()),
        Err(CodecError::DimensionMismatch { .. })
    ));
}

fn exercise_residual_index_search<S: SetStore>(idx: &Index<S>, model: &ResidualModel) {
    let prepared = model
        .prepare_query(&[1.0, -0.25, 0.5, 0.125])
        .expect("prepare query");
    let mut codes = Vec::new();
    {
        let mut writer = idx.writer();
        for document in 0..260usize {
            let signs: Vec<bool> = (0..RESIDUAL_SIGNS)
                .map(|bit| (bit * 17 + document * 13) % 29 < 14)
                .collect();
            let code =
                PackedCode::new(&signs, ((document * 311) as u16) & MAX_NORM_CODE).expect("pack");
            writer
                .put(DocId(document as u64), CodeRef::Dense(&code.to_words()))
                .expect("put");
            if document % 3 == 0 {
                writer.attr(DocId(document as u64), 9).expect("attr");
            }
            codes.push(code);
        }
        writer.commit().expect("commit");
    }

    let filter = Filter::And(vec![Filter::Term(9), Filter::IdRange(10, 250)]);
    let want = prepared.top_k(
        codes
            .iter()
            .enumerate()
            .filter(|(id, _)| *id % 3 == 0 && (10..250).contains(id))
            .map(|(id, code)| (DocId(id as u64), code)),
        17,
    );
    let got = prepared
        .search(idx)
        .expect("matching model-bound index")
        .filter(filter)
        .k(17)
        .execute()
        .expect("search");

    assert_eq!(got.scored, 80);
    assert_eq!(
        got.hits
            .iter()
            .map(|hit| haiiie_embed::ResidualHit {
                id: hit.id,
                score: hit.score,
            })
            .collect::<Vec<_>>(),
        want
    );
}

#[test]
fn residual_query_scores_filtered_persisted_rows_exactly() {
    let model = small_residual_model();
    let memory =
        Index::create_with_model_id(MemStore::new(), 10, 512, model.id().as_bytes()).expect("mem");
    exercise_residual_index_search(&memory, &model);

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db");
    let stored = Index::create_with_model_id(
        YesnoStore::open(&path).expect("open store"),
        11,
        512,
        model.id().as_bytes(),
    )
    .expect("create");
    exercise_residual_index_search(&stored, &model);
    stored.store().flush().expect("checkpoint");
    drop(stored);

    let reopened = Index::open(YesnoStore::open(&path).expect("reopen store"), 11).expect("index");
    let prepared = model
        .prepare_query(&[-0.5, 0.25, 1.0, -0.125])
        .expect("prepare");
    let got = prepared
        .search(&reopened)
        .expect("identity survived reopen")
        .filter(Filter::Term(9))
        .k(7)
        .execute()
        .expect("persisted search");
    assert_eq!(got.hits.len(), 7);
    assert_eq!(got.scored, 87);

    let foreign =
        Index::create_with_model_id(MemStore::new(), 12, 512, [0x77; 32]).expect("foreign");
    assert!(matches!(
        prepared.search(&foreign),
        Err(CodecError::ModelMismatch { .. })
    ));
}

fn assert_forward_only<S: SetStore>(index: &Index<S>) {
    assert!(index.meta().is_residual(), "layout tag was lost on reopen");
    let snapshot = index.store().snapshot().expect("snapshot");
    let keys = index.keys();
    for dimension in 0..index.meta().dims {
        assert!(
            snapshot
                .load(keys.dim(dimension))
                .expect("dimension key")
                .is_empty(),
            "DIM {dimension} was stored for a residual index"
        );
    }
    for plane in 0..index.meta().z_planes() {
        assert!(
            snapshot
                .load(keys.zplane(plane))
                .expect("weight plane")
                .is_empty(),
            "ZPLANE {plane} was stored for a residual index"
        );
    }
    assert!(
        snapshot
            .load(keys.stat(0))
            .expect("statistics key")
            .is_empty()
    );
}

fn assert_stored_code<S: SetStore>(index: &Index<S>, id: DocId, code: &PackedCode) {
    let snapshot = index.store().snapshot().expect("snapshot");
    let address = index.row_addr(id);
    let mut block = haiiie_core::slice::zero_mask();
    snapshot
        .load_block(index.keys().forward(), address.block, &mut block)
        .expect("forward block");
    let start = (address.base % haiiie_core::BLOCK_ORDINALS) as usize / 64;
    assert_eq!(&block[start..start + 8], &code.to_words());
}

#[test]
fn forward_only_residual_rows_survive_reopen_churn_and_compaction() {
    let model = small_residual_model();
    let code = |seed: usize| {
        let signs: Vec<_> = (0..RESIDUAL_SIGNS)
            .map(|bit| (bit * 17 + seed * 11) % 29 < 14)
            .collect();
        PackedCode::new(&signs, (seed * 311) as u16).expect("pack")
    };
    let first = code(1);
    let second = code(2);
    let replacement = code(3);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db");
    let namespace = 13;
    let index = Index::create_with_model_id(
        YesnoStore::open(&path).expect("store"),
        namespace,
        512,
        model.id().as_bytes(),
    )
    .expect("create");
    assert!(index.meta().is_residual());
    {
        let mut writer = index.writer();
        writer
            .put(DocId(7), CodeRef::Dense(&first.to_words()))
            .expect("first put")
            .attr(DocId(7), 9)
            .expect("attribute")
            .put(DocId(1000), CodeRef::Dense(&second.to_words()))
            .expect("second put");
        let forward_bits: u32 = first
            .to_words()
            .iter()
            .chain(second.to_words().iter())
            .map(|word| word.count_ones())
            .sum();
        assert_eq!(writer.pending(), forward_bits as usize + 3);
        writer.commit().expect("commit");
    }
    assert_forward_only(&index);
    let version = index.store().snapshot().expect("snapshot").version();
    assert_eq!(
        index.refresh_stats().expect("residual stats no-op"),
        version
    );
    assert_eq!(
        index.store().snapshot().expect("snapshot").version(),
        version
    );
    assert!(matches!(
        index
            .search()
            .code(CodeRef::Dense(&first.to_words()))
            .execute(),
        Err(Error::BinarySearchOnResidualIndex)
    ));
    assert!(matches!(
        index.search().explain(),
        Err(Error::BinarySearchOnResidualIndex)
    ));
    assert_eq!(index.search().count().expect("count"), 2);
    index.store().flush().expect("checkpoint");
    drop(index);

    let reopened =
        Index::open(YesnoStore::open(&path).expect("reopen"), namespace).expect("reopen index");
    assert_forward_only(&reopened);
    assert_stored_code(&reopened, DocId(7), &first);
    assert_stored_code(&reopened, DocId(1000), &second);
    {
        let mut writer = reopened.writer();
        writer
            .put(DocId(7), CodeRef::Dense(&replacement.to_words()))
            .expect("overwrite");
        writer.delete(DocId(1000)).expect("delete");
        writer
            .put(DocId(1200), CodeRef::Dense(&second.to_words()))
            .expect("new put");
        writer.commit().expect("churn commit");
    }
    // A delete followed by a put in the same batch must not difference the
    // put against the pre-delete snapshot: its clear is still pending.
    {
        let mut writer = reopened.writer();
        writer.delete(DocId(7)).expect("delete");
        writer
            .put(DocId(7), CodeRef::Dense(&replacement.to_words()))
            .expect("re-add in same batch");
        writer.commit().expect("re-add commit");
    }
    reopened.store().flush().expect("checkpoint churn");
    drop(reopened);

    let reopened = Index::open(YesnoStore::open(&path).expect("reopen churn"), namespace)
        .expect("reopen churn index");
    assert_forward_only(&reopened);
    assert_stored_code(&reopened, DocId(7), &replacement);
    assert_stored_code(&reopened, DocId(1200), &second);
    let prepared = model
        .prepare_query(&[1.0, -0.25, 0.5, 0.125])
        .expect("prepare");
    let expected = prepared.top_k([(DocId(7), &replacement), (DocId(1200), &second)], 2);
    let hits = prepared
        .search(&reopened)
        .expect("matching model")
        .k(2)
        .execute()
        .expect("exact search");
    assert_eq!(
        hits.hits
            .iter()
            .map(|hit| (hit.id, hit.score))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|hit| (hit.id, hit.score))
            .collect::<Vec<_>>()
    );
    let filtered = prepared
        .search(&reopened)
        .expect("matching model")
        .filter(Filter::Term(9))
        .execute()
        .expect("filtered search");
    assert_eq!(filtered.hits.len(), 1);
    assert_eq!(filtered.hits[0].id, DocId(7));
    assert_eq!(filtered.hits[0].score, prepared.score(&replacement));
    drop(reopened);

    let mapping = compact(&path, namespace).expect("compact");
    assert_eq!(mapping.old_ids.len(), 2);
    assert!(mapping.old_ids.contains(&DocId(7)));
    assert!(mapping.old_ids.contains(&DocId(1200)));
    assert!(matches!(
        Index::open(YesnoStore::open(&path).expect("pending store"), namespace),
        Err(Error::CompactionPending(13))
    ));
    assert_eq!(compact(&path, namespace).expect("recover handoff"), mapping);
    acknowledge_compaction(&path, namespace, mapping.source_version).expect("acknowledge");
    let compacted = Index::open(YesnoStore::open(&path).expect("compacted store"), namespace)
        .expect("compacted index");
    assert_forward_only(&compacted);
    for (new_id, old_id) in mapping.old_ids.iter().enumerate() {
        let expected_code = if *old_id == DocId(7) {
            &replacement
        } else {
            &second
        };
        assert_stored_code(&compacted, DocId(new_id as u64), expected_code);
    }
    let expected = prepared.top_k(
        mapping.old_ids.iter().enumerate().map(|(new_id, old_id)| {
            (
                DocId(new_id as u64),
                if *old_id == DocId(7) {
                    &replacement
                } else {
                    &second
                },
            )
        }),
        2,
    );
    let hits = prepared
        .search(&compacted)
        .expect("matching compacted model")
        .k(2)
        .execute()
        .expect("compacted exact search");
    assert_eq!(
        hits.hits
            .iter()
            .map(|hit| (hit.id, hit.score))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|hit| (hit.id, hit.score))
            .collect::<Vec<_>>()
    );
}
