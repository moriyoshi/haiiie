//! `haiiie-fit` -- fit the supported fixed 500-sign residual model offline.
//!
//! The fitter consumes a row-major little-endian `f32` corpus. It follows the
//! frozen accuracy study: normalized fit rows, greedy two-entry residual atoms,
//! a ridge decoder, signed 12-bit decoder quantization, and a 12-bit reciprocal-
//! norm interval. The seed and source digest are printed with the output model
//! identity so a fit can be reproduced without retaining document floats.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::Parser;
use haiiie_embed::{MAX_NORM_CODE, RESIDUAL_SIGNS, ResidualModel, Rng};
use nalgebra::{DMatrix, DVector};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

// The frozen study used eight power iterations per greedy residual atom.
const POWER_ITERATIONS: usize = 8;
// The frozen decoder fit used lambda = fit_rows * 1e-4.
const RIDGE_RATE: f64 = 1e-4;
// The fixed codec assigns signed 12-bit magnitude to decoder coefficients.
const MAX_DECODER_COEFFICIENT: f64 = 2_047.0;

#[derive(Parser, Debug)]
#[command(
    name = "haiiie-fit",
    about = "Fit a fixed 500-sign residual model from raw f32 vectors"
)]
struct Args {
    /// Row-major little-endian f32 fit vectors, with no header.
    #[arg(long)]
    input: PathBuf,
    /// Components in each input vector.
    #[arg(long)]
    dims: usize,
    /// New model file to create. An existing file is never overwritten.
    #[arg(long)]
    output: PathBuf,
    /// Deterministic SplitMix64/Box-Muller initialization seed.
    #[arg(long)]
    seed: u64,
    /// Worker threads used during residual fitting.
    ///
    /// Four reproduces the worker width used by the frozen accuracy study.
    #[arg(long, default_value_t = 4)]
    threads: usize,
}

#[derive(Debug)]
struct Fit {
    model: ResidualModel,
    rows: usize,
    norm_offset: i64,
    norm_step: f64,
    clipped_fit_rows: usize,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn read_rows(path: &Path, dims: usize) -> io::Result<(Vec<f32>, usize, [u8; 32])> {
    if dims == 0 {
        return Err(invalid("--dims must be positive"));
    }
    let bytes = fs::read(path)?;
    let row_bytes = dims
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| invalid("row byte width overflows usize"))?;
    if bytes.is_empty() || bytes.len() % row_bytes != 0 {
        return Err(invalid(format!(
            "input has {} bytes; expected a positive multiple of {row_bytes}",
            bytes.len()
        )));
    }
    let rows = bytes.len() / row_bytes;
    let values = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("four checked bytes")))
        .collect();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    Ok((values, rows, digest))
}

fn normalize_rows(values: &[f32], rows: usize, dims: usize) -> io::Result<Vec<f64>> {
    if values.len() != rows.saturating_mul(dims) {
        return Err(invalid("row geometry does not match the input values"));
    }
    let mut normalized = Vec::with_capacity(values.len());
    for (row_index, row) in values.chunks_exact(dims).enumerate() {
        if row.iter().any(|value| !value.is_finite()) {
            return Err(invalid(format!(
                "row {row_index} contains a non-finite value"
            )));
        }
        let squared_norm: f64 = row
            .iter()
            .map(|&value| f64::from(value) * f64::from(value))
            .sum();
        if squared_norm == 0.0 || !squared_norm.is_finite() {
            return Err(invalid(format!(
                "row {row_index} does not have a finite positive norm"
            )));
        }
        let norm = squared_norm.sqrt();
        // The study normalized to f32 before fitting in f64. Keeping that cast
        // is part of the model construction, rather than an avoidable loss here.
        normalized.extend(
            row.iter()
                .map(|&value| f64::from((f64::from(value) / norm) as f32)),
        );
    }
    Ok(normalized)
}

fn vector_norm(vector: &DVector<f64>) -> f64 {
    vector.iter().map(|value| value * value).sum::<f64>().sqrt()
}

fn train_encoder(
    normalized: &[f64],
    rows: usize,
    dims: usize,
    signs: usize,
    seed: u64,
) -> io::Result<(Vec<i8>, Vec<f32>)> {
    let matrix = DMatrix::from_row_slice(rows, dims, normalized);
    let mut covariance = matrix.transpose() * matrix;
    let mut residual = normalized.to_vec();
    let mut codes = vec![0i8; rows * signs];
    let mut encoder = Vec::with_capacity(signs * dims);
    let mut rng = Rng::new(seed);

    for sign_index in 0..signs {
        let mut axis =
            DVector::from_iterator(dims, (0..dims).map(|_| f64::from(rng.next_normal())));
        let initial_norm = vector_norm(&axis);
        if initial_norm == 0.0 || !initial_norm.is_finite() {
            return Err(invalid(
                "random initialization did not have a finite positive norm",
            ));
        }
        axis /= initial_norm;
        for _ in 0..POWER_ITERATIONS {
            axis = &covariance * axis;
            let norm = vector_norm(&axis).max(1e-20);
            axis /= norm;
        }

        let mut row_signs = vec![1i8; rows];
        row_signs
            .par_iter_mut()
            .enumerate()
            .for_each(|(row_index, sign)| {
                let row = &residual[row_index * dims..(row_index + 1) * dims];
                let dot: f64 = row.iter().zip(axis.iter()).map(|(a, b)| a * b).sum();
                *sign = if dot >= 0.0 { 1 } else { -1 };
            });

        // Each output dimension is independent and sums rows in fixed order.
        // That makes model bytes independent of the Rayon scheduling order.
        let mut atom = vec![0.0f64; dims];
        atom.par_iter_mut()
            .enumerate()
            .for_each(|(dimension, value)| {
                let mut sum = 0.0;
                for row_index in 0..rows {
                    sum += f64::from(row_signs[row_index]) * residual[row_index * dims + dimension];
                }
                *value = sum / rows as f64;
            });
        if atom.iter().all(|&value| value == 0.0) || atom.iter().any(|v| !v.is_finite()) {
            return Err(invalid(format!(
                "residual collapsed while fitting atom {sign_index}; use a larger or more varied fit corpus"
            )));
        }

        residual
            .par_chunks_mut(dims)
            .zip(row_signs.par_iter())
            .for_each(|(row, &sign)| {
                let direction = f64::from(sign);
                for (value, coefficient) in row.iter_mut().zip(&atom) {
                    *value -= direction * coefficient;
                }
            });
        for (row_index, &sign) in row_signs.iter().enumerate() {
            codes[row_index * signs + sign_index] = sign;
        }

        let row_scale = rows as f64;
        for left in 0..dims {
            for right in 0..dims {
                covariance[(left, right)] -= row_scale * atom[left] * atom[right];
            }
        }
        for left in 0..dims {
            for right in 0..left {
                let average = (covariance[(left, right)] + covariance[(right, left)]) * 0.5;
                covariance[(left, right)] = average;
                covariance[(right, left)] = average;
            }
        }
        encoder.extend(atom.into_iter().map(|value| value as f32));
        if signs == RESIDUAL_SIGNS && (sign_index + 1) % 50 == 0 {
            eprintln!(
                "fitted {} / {RESIDUAL_SIGNS} residual signs",
                sign_index + 1
            );
        }
    }
    Ok((codes, encoder))
}

fn fit_decoder(
    codes: &[i8],
    normalized: &[f64],
    rows: usize,
    dims: usize,
    signs: usize,
) -> io::Result<Vec<i16>> {
    let code_matrix = DMatrix::from_fn(rows, signs, |row, sign| {
        f64::from(codes[row * signs + sign])
    });
    let source_matrix = DMatrix::from_row_slice(rows, dims, normalized);
    let mut gram = code_matrix.transpose() * &code_matrix;
    let regularization = rows as f64 * RIDGE_RATE;
    for diagonal in 0..signs {
        gram[(diagonal, diagonal)] += regularization;
    }
    let right = code_matrix.transpose() * source_matrix;
    let cholesky = gram
        .cholesky()
        .ok_or_else(|| invalid("regularized decoder system is not positive definite"))?;
    let decoder = cholesky.solve(&right);
    let mut decoder_f32 = Vec::with_capacity(signs * dims);
    for sign in 0..signs {
        for dimension in 0..dims {
            decoder_f32.push(decoder[(sign, dimension)] as f32);
        }
    }
    let largest = decoder_f32
        .iter()
        .map(|value| f64::from(value.abs()))
        .fold(0.0, f64::max);
    if largest == 0.0 || !largest.is_finite() {
        return Err(invalid("ridge decoder has no finite non-zero coefficient"));
    }
    let scale = MAX_DECODER_COEFFICIENT / largest;
    Ok(decoder_f32
        .into_iter()
        .map(|value| (f64::from(value) * scale).round_ties_even() as i16)
        .collect())
}

fn calibrate_norms(
    codes: &[i8],
    decoder: &[i16],
    rows: usize,
    dims: usize,
    signs: usize,
) -> io::Result<(i64, f64, usize)> {
    let mut inverse = Vec::with_capacity(rows);
    for row_index in 0..rows {
        let mut squared_norm = 0i64;
        for dimension in 0..dims {
            let decoded: i64 = (0..signs)
                .map(|sign| {
                    i64::from(codes[row_index * signs + sign])
                        * i64::from(decoder[sign * dims + dimension])
                })
                .sum();
            squared_norm = squared_norm
                .checked_add(
                    decoded
                        .checked_mul(decoded)
                        .ok_or_else(|| invalid("decoded norm overflows i64"))?,
                )
                .ok_or_else(|| invalid("decoded norm overflows i64"))?;
        }
        if squared_norm == 0 {
            return Err(invalid(format!("fit row {row_index} decodes to zero")));
        }
        inverse.push(1.0 / (squared_norm as f64).sqrt());
    }
    let minimum = inverse.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = inverse.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    // The study left one quantization interval of headroom at each endpoint.
    let step = (maximum - minimum) / (f64::from(MAX_NORM_CODE) - 1.0);
    if !step.is_finite() || step <= 0.0 {
        return Err(invalid(
            "fit rows do not span a usable reciprocal-norm interval",
        ));
    }
    let offset = (minimum / step).round_ties_even() as i64;
    let clipped = inverse
        .iter()
        .filter(|&&value| {
            let raw = (value / step).round_ties_even() - offset as f64;
            raw < 0.0 || raw > f64::from(MAX_NORM_CODE)
        })
        .count();
    Ok((offset, step, clipped))
}

fn fit_model(
    values: &[f32],
    rows: usize,
    dims: usize,
    seed: u64,
) -> Result<Fit, Box<dyn std::error::Error + Send + Sync>> {
    let normalized = normalize_rows(values, rows, dims)?;
    let (codes, encoder) = train_encoder(&normalized, rows, dims, RESIDUAL_SIGNS, seed)?;
    let decoder = fit_decoder(&codes, &normalized, rows, dims, RESIDUAL_SIGNS)?;
    let (norm_offset, norm_step, clipped_fit_rows) =
        calibrate_norms(&codes, &decoder, rows, dims, RESIDUAL_SIGNS)?;
    let model = ResidualModel::new(
        u32::try_from(dims).map_err(|_| invalid("--dims does not fit u32"))?,
        norm_offset,
        norm_step,
        encoder,
        decoder,
    )?;
    Ok(Fit {
        model,
        rows,
        norm_offset,
        norm_step,
        clipped_fit_rows,
    })
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    if args.threads == 0 {
        return Err(invalid("--threads must be positive").into());
    }
    let (values, rows, source_digest) = read_rows(&args.input, args.dims)?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .build()?;
    let fit = pool.install(|| fit_model(&values, rows, args.dims, args.seed))?;
    let model_bytes = fit.model.to_bytes();
    write_new(&args.output, &model_bytes)?;
    println!("model_id          {}", fit.model.id());
    println!("source_sha256      {}", hex(&source_digest));
    println!("fit_rows           {}", fit.rows);
    println!("dimensions         {}", args.dims);
    println!("seed               {}", args.seed);
    println!("threads            {}", args.threads);
    println!("norm_offset        {}", fit.norm_offset);
    println!("norm_step          {:.17e}", fit.norm_step);
    println!("clipped_fit_rows   {}", fit.clipped_fit_rows);
    println!("model_bytes        {}", model_bytes.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitting_stages_produce_a_bounded_nonzero_decoder() {
        let rows = 96;
        let dims = 7;
        let signs = 12;
        let mut rng = Rng::new(91);
        let values: Vec<f32> = (0..rows * dims).map(|_| rng.next_normal()).collect();
        let normalized = normalize_rows(&values, rows, dims).unwrap();
        let one = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let four = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let (codes, encoder) = one
            .install(|| train_encoder(&normalized, rows, dims, signs, 7))
            .unwrap();
        let (parallel_codes, parallel_encoder) = four
            .install(|| train_encoder(&normalized, rows, dims, signs, 7))
            .unwrap();
        assert_eq!(parallel_codes, codes);
        assert_eq!(parallel_encoder, encoder);
        let decoder = fit_decoder(&codes, &normalized, rows, dims, signs).unwrap();
        let (offset, step, _) = calibrate_norms(&codes, &decoder, rows, dims, signs).unwrap();

        assert_eq!(encoder.len(), signs * dims);
        assert_eq!(decoder.len(), signs * dims);
        assert!(decoder.iter().any(|&value| value != 0));
        assert!(decoder.iter().all(|&value| value.abs() <= 2_047));
        assert!(offset > 0);
        assert!(step.is_finite() && step > 0.0);
    }

    #[test]
    fn decoder_is_flattened_by_sign_then_dimension() {
        let codes = [1, 1, 1, -1, -1, 1, -1, -1];
        let expected = [[0.8, 0.1, 0.2], [0.1, 0.7, -0.3]];
        let mut source = Vec::new();
        for row in codes.chunks_exact(2) {
            for (&first, &second) in expected[0].iter().zip(&expected[1]) {
                source.push(f64::from(row[0]) * first + f64::from(row[1]) * second);
            }
        }
        let decoder = fit_decoder(&codes, &source, 4, 3, 2).unwrap();

        assert!(decoder[0] > decoder[1] && decoder[0] > decoder[2]);
        assert!(decoder[4] > decoder[3] && decoder[4] > decoder[5].abs());
        assert!(decoder[5] < 0);
    }

    #[test]
    fn normalization_refuses_a_zero_row() {
        let error = normalize_rows(&[1.0, 0.0, 0.0, 0.0], 2, 2).unwrap_err();
        assert!(error.to_string().contains("row 1"));
    }
}
