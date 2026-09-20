//! Wire types to engine types, and back.
//!
//! # Every conversion is total or it is an error
//!
//! Protobuf has no required fields and every enum may be zero. This module
//! turns each boundary value into either a valid engine value or a `Status`;
//! it never guesses a missing query representation or metric.

#![allow(clippy::result_large_err)]

use haiiie_core::{CodeRef, Consistency, DocId, Filter, Hit, Metric, Score};
use haiiie_proto::v1;
use tonic::Status;

/// A query code together with the allocation that owns its words or positions.
pub enum OwnedCode {
    Dense(Vec<u64>),
    Sparse(Vec<u32>),
}

impl OwnedCode {
    #[must_use]
    pub fn as_ref(&self) -> CodeRef<'_> {
        match self {
            Self::Dense(words) => CodeRef::Dense(words),
            Self::Sparse(positions) => CodeRef::Sparse(positions),
        }
    }
}

pub enum OwnedQuery {
    Binary { code: OwnedCode, metric: Metric },
    Residual(Vec<f32>),
}

pub fn code(code: Option<&v1::Code>) -> Result<OwnedCode, Status> {
    let representation = code
        .and_then(|code| code.representation.as_ref())
        .ok_or_else(|| Status::invalid_argument("code representation is missing"))?;
    Ok(match representation {
        v1::code::Representation::Dense(dense) => OwnedCode::Dense(dense.words.clone()),
        v1::code::Representation::Sparse(sparse) => {
            if sparse.positions.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(Status::invalid_argument(
                    "sparse positions must be strictly ascending",
                ));
            }
            OwnedCode::Sparse(sparse.positions.clone())
        }
    })
}

pub fn query(query: Option<&v1::Query>) -> Result<OwnedQuery, Status> {
    let kind = query
        .and_then(|query| query.kind.as_ref())
        .ok_or_else(|| Status::invalid_argument("query is missing"))?;
    match kind {
        v1::query::Kind::Binary(binary) => Ok(OwnedQuery::Binary {
            code: code(binary.code.as_ref())?,
            metric: metric(binary.metric)?,
        }),
        v1::query::Kind::Residual(residual) => {
            let embedding = residual
                .embedding
                .as_ref()
                .ok_or_else(|| Status::invalid_argument("residual embedding is missing"))?;
            Ok(OwnedQuery::Residual(embedding.values.clone()))
        }
    }
}

pub fn metric(metric: i32) -> Result<Metric, Status> {
    match v1::Metric::try_from(metric) {
        Ok(v1::Metric::Dot) => Ok(Metric::Dot),
        Ok(v1::Metric::Hamming) => Ok(Metric::Hamming),
        Ok(v1::Metric::Jaccard) => Ok(Metric::Jaccard),
        Ok(v1::Metric::Cosine) => Ok(Metric::Cosine),
        Ok(v1::Metric::Unspecified) => Err(Status::invalid_argument(
            "metric is unspecified; the server will not guess which question was asked",
        )),
        Err(_) => Err(Status::invalid_argument("unknown metric")),
    }
}

pub fn consistency(policy: Option<&v1::ScanPolicy>) -> Result<Consistency, Status> {
    let Some(policy) = policy else {
        return Ok(Consistency::ResumeOnEviction { max_retries: 3 });
    };
    match v1::Consistency::try_from(policy.consistency) {
        Ok(v1::Consistency::Strict) => Ok(Consistency::Strict),
        Ok(v1::Consistency::Unspecified | v1::Consistency::ResumeOnEviction) => {
            Ok(Consistency::ResumeOnEviction {
                max_retries: if policy.max_retries == 0 {
                    3
                } else {
                    policy.max_retries
                },
            })
        }
        Err(_) => Err(Status::invalid_argument("unknown consistency policy")),
    }
}

pub fn filter(wire_filter: Option<&v1::Filter>) -> Result<Filter, Status> {
    let Some(message) = wire_filter else {
        return Ok(Filter::All);
    };
    let kind = message
        .kind
        .as_ref()
        .ok_or_else(|| Status::invalid_argument("filter kind is missing"))?;
    Ok(match kind {
        v1::filter::Kind::All(_) => Filter::All,
        v1::filter::Kind::None(_) => Filter::None,
        v1::filter::Kind::Term(term) => Filter::Term(*term),
        v1::filter::Kind::Ids(list) => Filter::Ids(list.ids.iter().copied().map(DocId).collect()),
        v1::filter::Kind::IdRange(range) => Filter::IdRange(range.lo, range.hi),
        v1::filter::Kind::And(clauses) => Filter::And(convert_clauses(&clauses.clauses)?),
        v1::filter::Kind::Or(clauses) => Filter::Or(convert_clauses(&clauses.clauses)?),
        v1::filter::Kind::Not(inner) => Filter::Not(Box::new(filter(Some(inner))?)),
    })
}

fn convert_clauses(clauses: &[v1::Filter]) -> Result<Vec<Filter>, Status> {
    clauses.iter().map(|clause| filter(Some(clause))).collect()
}

#[must_use]
pub fn binary_hit(hit: &Hit) -> v1::SearchHit {
    let (numerator, denominator, square_root) = match hit.score {
        Score::Int(value) => (value, 1, false),
        Score::Ratio { num, den } => (num as i64, den, false),
        Score::RatioSq { num, den } => (num as i64, den as u64, true),
    };
    v1::SearchHit {
        id: hit.id.get(),
        score: Some(v1::search_hit::Score::Binary(v1::BinaryScore {
            intersection: hit.inter,
            document_weight: hit.weight,
            exact: Some(v1::ExactScore {
                numerator,
                denominator,
                square_root,
            }),
            approximate: hit.score.as_f64(),
        })),
    }
}

#[must_use]
pub fn residual_hit(hit: &haiiie_core::RowHit) -> v1::SearchHit {
    v1::SearchHit {
        id: hit.id.get(),
        score: Some(v1::search_hit::Score::Residual(v1::ResidualScore {
            value: hit.score,
        })),
    }
}
