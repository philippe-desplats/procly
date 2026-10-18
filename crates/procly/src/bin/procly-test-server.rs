//! A tiny HTTP server for the integration tests, so the suite needs no Node or Python toolchain.
//!
//! It listens on `127.0.0.1` at the port given by `--port <PORT>`, or else by the `PORT` environment variable,
//! answers `ok` to every request, with the status `<STATUS>` to a request for `/status/<STATUS>` and `200` to any
//! other, and exits with code `<CODE>` after answering a request for `/exit/<CODE>`.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;

fn main() -> ExitCode {
    let port = match port() {
        Ok(port) => port,
        Err(message) => {
            eprintln!("procly-test-server: {message}");
            return ExitCode::from(2);
        }
    };
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("procly-test-server: cannot listen on port {port}: {error}");
            return ExitCode::from(2);
        }
    };
    println!("listening on {port}");

    for stream in listener.incoming().flatten() {
        if let Some(code) = answer(stream) {
            println!("exiting with {code}");
            return ExitCode::from(code);
        }
    }
    ExitCode::SUCCESS
}

fn port() -> Result<u16, String> {
    let mut args = std::env::args().skip(1);
    let port = match (args.next().as_deref(), args.next()) {
        (Some("--port"), Some(port)) => port,
        (None, _) => std::env::var("PORT").map_err(|_| "no port given, pass --port <PORT> or set PORT".to_owned())?,
        _ => return Err("usage: procly-test-server [--port <PORT>]".to_owned()),
    };
    port.parse().map_err(|_| format!("invalid port {port:?}"))
}

/// Answers one request, and returns the exit code it asked for, if any.
fn answer(mut stream: TcpStream) -> Option<u8> {
    let mut request_line = String::new();
    BufReader::new(&stream).read_line(&mut request_line).ok()?;
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let exit_code = path.strip_prefix("/exit/").and_then(|code| code.parse().ok());
    let status = match path
        .strip_prefix("/status/")
        .and_then(|status| status.parse::<u16>().ok())
    {
        Some(status) => format!("{status} Status"),
        None => "200 OK".to_owned(),
    };
    let response = format!("HTTP/1.1 {status}\r\nContent-Length: 3\r\nConnection: close\r\n\r\nok\n");
    let _ = stream.write_all(response.as_bytes());
    exit_code
}
