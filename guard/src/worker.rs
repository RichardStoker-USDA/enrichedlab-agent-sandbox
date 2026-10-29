//! The bounded worker: `enrichedlab-guard worker TASK ...`, started by the service for every step that parses an
//! uploaded file, under the helper limits (address space, CPU, wall time, output size). It reads one file, writes
//! its report as one JSON object to standard output, and exits. A file whose objects cannot be read exits with
//! `pdf::UNREADABLE_EXIT`; any other failure exits with 1 and one line on standard error that names no content.
//!
//! - `pdf-inspect --input PDF --rules DIR`: active content, hidden layers, metadata counts, stream uses and the rule
//!   matches in decoded streams and action text (`pdf::Report`).
//! - `pdf-text --input PDF`: the hidden-text counts (`pdf::Hidden`).
//! - `pdf-strip --input PDF --output PDF`: the rebuild's object pass and canonical content (`pdf::Stripped`).
//! - `raster --format png|jpeg --input FILE --output FILE`: the decoded and re-encoded image (`{"bytes": N}`).
//! - `dataset-grid --type delimited|xlsx --input FILE --output FILE --rules DIR`: a lab dataset's checks and its cell
//!   grid, written when the dataset is within its bounds (`dataset::Report`).
use crate::dataset;
use crate::pdf;
use crate::raster;
use crate::rules::Rules;
use anyhow::{Context, Result, bail};
use image::ImageFormat;
use serde::Serialize;
use std::path::PathBuf;
use std::process::ExitCode;

fn flag(args: &[String], name: &str) -> Result<PathBuf> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .with_context(|| format!("{name} is required"))
}

/// A file the worker could not read as a PDF, as opposed to a worker failure.
struct Unreadable;

fn document(args: &[String]) -> Result<Result<lopdf::Document, Unreadable>> {
    let path = flag(args, "--input")?;
    Ok(pdf::load(&path).map_err(|_| Unreadable))
}

fn emit<T: Serialize>(value: &T) -> Result<Result<String, Unreadable>> {
    Ok(Ok(serde_json::to_string(value)?))
}

fn task(args: &[String]) -> Result<Result<String, Unreadable>> {
    match args.first().map(String::as_str) {
        Some("pdf-inspect") => {
            let rules = Rules::load(&flag(args, "--rules")?)?;
            match document(args)? {
                Ok(document) => emit(&pdf::report(&document, &rules)),
                Err(unreadable) => Ok(Err(unreadable)),
            }
        }
        Some("pdf-text") => match document(args)? {
            Ok(document) => emit(&pdf::hidden_text(&document)),
            Err(unreadable) => Ok(Err(unreadable)),
        },
        Some("pdf-strip") => {
            let output = flag(args, "--output")?;
            match document(args)? {
                Ok(mut document) => {
                    let stripped = pdf::rebuild_objects(&mut document)?;
                    document.save(&output).context("stripped PDF not written")?;
                    emit(&stripped)
                }
                Err(unreadable) => Ok(Err(unreadable)),
            }
        }
        Some("raster") => {
            let format = match args
                .iter()
                .position(|a| a == "--format")
                .and_then(|i| args.get(i + 1))
                .map(String::as_str)
            {
                Some("png") => ImageFormat::Png,
                Some("jpeg") => ImageFormat::Jpeg,
                _ => bail!("--format must be png or jpeg"),
            };
            let bytes = std::fs::read(flag(args, "--input")?).context("image unreadable")?;
            let rebuilt = raster::reencode(&bytes, format)?;
            std::fs::write(flag(args, "--output")?, &rebuilt).context("image not written")?;
            emit(&serde_json::json!({"bytes": rebuilt.len()}))
        }
        Some("dataset-grid") => {
            let source = args
                .iter()
                .position(|a| a == "--type")
                .and_then(|i| args.get(i + 1))
                .and_then(|t| dataset::Source::from_arg(t))
                .context("--type must be delimited or xlsx")?;
            let rules = Rules::load(&flag(args, "--rules")?)?;
            let bytes = std::fs::read(flag(args, "--input")?).context("dataset unreadable")?;
            let max_copy = crate::store::rebuilt_limit(bytes.len() as u64);
            match dataset::rebuild(&bytes, source, &rules, max_copy, dataset::Limits::default())? {
                Ok(rebuilt) => {
                    if let Some(copy) = &rebuilt.copy {
                        std::fs::write(flag(args, "--output")?, copy)
                            .context("grid not written")?;
                    }
                    emit(&rebuilt.report)
                }
                Err(dataset::Unreadable) => Ok(Err(Unreadable)),
            }
        }
        _ => bail!("unknown worker task"),
    }
}

pub fn main(args: &[String]) -> ExitCode {
    match task(args) {
        Ok(Ok(report)) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Ok(Err(Unreadable)) => ExitCode::from(pdf::UNREADABLE_EXIT as u8),
        Err(e) => {
            eprintln!("{}", format!("{e:#}").lines().next().unwrap_or("failed"));
            ExitCode::FAILURE
        }
    }
}
