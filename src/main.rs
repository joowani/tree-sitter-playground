//! Web playground that shows the Tree-sitter AST of code typed into a browser editor.

use std::time::Instant;

use anyhow::Context;
use axum::extract::DefaultBodyLimit;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tree_sitter_playground::{
    ParseError, SUPPORTED_LANGUAGES, auto_detect, count_errors, format_ast, node_label, parse,
    resolve_language, walk_ast,
};

const INDEX_HTML: &str = include_str!("../web/index.html");

/// The largest request the playground accepts. Parse time, the AST, and the node list all grow with the code, and
/// 256 KiB covers any real source file.
const MAX_REQUEST_BYTES: usize = 256 * 1024;

#[derive(Parser)]
#[command(
    name = "tree-sitter-playground",
    version,
    about = "Web playground for Tree-sitter ASTs"
)]
struct Cli {
    /// Host name or IP address to bind to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to listen on, or 0 for any free port
    #[arg(long, default_value_t = 3000)]
    port: u16,
}

#[derive(Deserialize)]
struct AstRequest {
    /// A supported language or alias, or `auto` to detect the language.
    language: String,
    code: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AstResponse {
    language: &'static str,
    /// The AST as text, one node per line.
    ast: String,
    /// The AST's nodes in the same order as the lines of `ast`.
    nodes: Vec<AstNode>,
    error_count: usize,
    parse_millis: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AstNode {
    depth: usize,
    field: Option<String>,
    label: String,
    is_error: bool,
    /// Zero-based `[row, column]`, with the column in bytes as Tree-sitter reports it.
    start: [usize; 2],
    end: [usize; 2],
    /// Offsets in UTF-16 code units, which is how the browser editor addresses text.
    from: usize,
    to: usize,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

async fn post_ast(request: Result<Json<AstRequest>, JsonRejection>) -> Response {
    let request = match request {
        Ok(Json(request)) => request,
        Err(rejection) => return rejection_response(&rejection),
    };
    match tokio::task::spawn_blocking(move || build_ast_response(&request)).await {
        Ok(Ok(response)) => Json(response).into_response(),
        Ok(Err(error)) => {
            let status = match error {
                ParseError::UnsupportedLanguage(_) => StatusCode::BAD_REQUEST,
                ParseError::CodeTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
                ParseError::Timeout | ParseError::AstTooLarge => StatusCode::UNPROCESSABLE_ENTITY,
                ParseError::IncompatibleGrammar(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            error_response(status, error.to_string())
        }
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

fn error_response(status: StatusCode, error: String) -> Response {
    (status, Json(ErrorResponse { error })).into_response()
}

/// Reports a request body that could not be read as JSON in the same shape as the handler's own errors.
fn rejection_response(rejection: &JsonRejection) -> Response {
    let status = rejection.status();
    let error = if status == StatusCode::PAYLOAD_TOO_LARGE {
        format!(
            "the request is larger than {} KiB; parse a smaller snippet",
            MAX_REQUEST_BYTES >> 10
        )
    } else {
        rejection.body_text()
    };
    error_response(status, error)
}

fn build_ast_response(request: &AstRequest) -> Result<AstResponse, ParseError> {
    let started = Instant::now();
    let (language, tree) = if request.language == "auto" {
        auto_detect(&request.code)?
    } else {
        let language = resolve_language(&request.language)
            .ok_or_else(|| ParseError::UnsupportedLanguage(request.language.clone()))?;
        (language, parse(language, &request.code)?)
    };
    let parse_millis = started.elapsed().as_secs_f64() * 1000.0;

    let root = tree.root_node();
    let ast = format_ast(root)?;
    let utf16_offsets = utf16_offsets(&request.code);
    let mut nodes = Vec::new();
    walk_ast(root, |node, field, depth| {
        let (start, end) = (node.start_position(), node.end_position());
        nodes.push(AstNode {
            depth,
            field: field.map(str::to_string),
            label: node_label(node),
            is_error: node.is_error() || node.is_missing(),
            start: [start.row, start.column],
            end: [end.row, end.column],
            from: utf16_offsets[node.start_byte()],
            to: utf16_offsets[node.end_byte()],
        });
    });
    Ok(AstResponse {
        language,
        ast,
        nodes,
        error_count: count_errors(root),
        parse_millis,
    })
}

/// Maps each byte offset that starts a character, and the end of `code`, to its offset in UTF-16 code units.
fn utf16_offsets(code: &str) -> Vec<usize> {
    let mut offsets = vec![0; code.len() + 1];
    let mut utf16_offset = 0;
    for (byte_offset, character) in code.char_indices() {
        offsets[byte_offset] = utf16_offset;
        utf16_offset += character.len_utf16();
    }
    offsets[code.len()] = utf16_offset;
    offsets
}

async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let app = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route(
            "/api/languages",
            get(|| async { Json(SUPPORTED_LANGUAGES) }),
        )
        .route("/api/ast", post(post_ast))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES));

    // Binding to `(host, port)` rather than a parsed `host:port` accepts IPv6 addresses and host names such as
    // `localhost`.
    let (host, port) = (cli.host.as_str(), cli.port);
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .with_context(|| format!("could not bind to --host {host} --port {port}"))?;
    println!("Playground running at http://{}", listener.local_addr()?);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}
