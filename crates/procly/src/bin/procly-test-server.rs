//! A tiny HTTP server for the integration tests, so the suite needs no Node or Python toolchain.
//!
//! It listens on `127.0.0.1` at the port given by `--port <PORT>`, or else by the `PORT` environment variable,
//! answers `ok` to every request, with the status `<STATUS>` to a request for `/status/<STATUS>`, `200` to a request
//! for `/host/<HOST>` whose `Host` header is `<HOST>` and `421` when it is not, `400` to a request target that is not
//! a path, and `200` to any other, and exits with code `<CODE>` after answering a request for `/exit/<CODE>`.
//!
//! `--also <PORT>`, which may be repeated, listens on that port too, and before the main one, as a debugger or an HMR
//! socket would. `--after <MS>` waits that many milliseconds once those are listened on, before the main port is.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

/// What the command line asks for.
struct Options {
    port: u16,
    also: Vec<u16>,
    after: Duration,
}

fn main() -> ExitCode {
    let options = match options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("procly-test-server: {message}");
            return ExitCode::from(2);
        }
    };
    for port in options.also {
        let Some(listener) = listen(port) else {
            return ExitCode::from(2);
        };
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                answer(stream);
            }
        });
    }
    std::thread::sleep(options.after);
    let Some(listener) = listen(options.port) else {
        return ExitCode::from(2);
    };

    for stream in listener.incoming().flatten() {
        if let Some(code) = answer(stream) {
            println!("exiting with {code}");
            return ExitCode::from(code);
        }
    }
    ExitCode::SUCCESS
}

/// Listens on `port`, saying so on stdout, or says on stderr why it cannot.
fn listen(port: u16) -> Option<TcpListener> {
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            println!("listening on {port}");
            Some(listener)
        }
        Err(error) => {
            eprintln!("procly-test-server: cannot listen on port {port}: {error}");
            None
        }
    }
}

fn options() -> Result<Options, String> {
    const USAGE: &str = "usage: procly-test-server [--port <PORT>] [--also <PORT>]... [--after <MS>]";
    let number = |flag: &str, value: Option<String>| -> Result<String, String> {
        value.ok_or_else(|| format!("{flag} needs a value, {USAGE}"))
    };
    let mut port = None;
    let mut also = Vec::new();
    let mut after = Duration::ZERO;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--port" => port = Some(number("--port", args.next())?),
            "--also" => also.push(parse_port(&number("--also", args.next())?)?),
            "--after" => {
                let ms = number("--after", args.next())?;
                after = Duration::from_millis(ms.parse().map_err(|_| format!("invalid delay {ms:?}"))?);
            }
            _ => return Err(USAGE.to_owned()),
        }
    }
    let port = match port {
        Some(port) => port,
        None => std::env::var("PORT").map_err(|_| "no port given, pass --port <PORT> or set PORT".to_owned())?,
    };
    Ok(Options {
        port: parse_port(&port)?,
        also,
        after,
    })
}

fn parse_port(port: &str) -> Result<u16, String> {
    port.parse().map_err(|_| format!("invalid port {port:?}"))
}

/// Answers one request, and returns the exit code it asked for, if any.
fn answer(mut stream: TcpStream) -> Option<u8> {
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let path = request_line.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let exit_code = path.strip_prefix("/exit/").and_then(|code| code.parse().ok());
    let status = if !path.starts_with('/') {
        "400 Bad Request".to_owned()
    } else if let Some(expected) = path.strip_prefix("/host/") {
        if host(&mut reader).as_deref() == Some(expected) {
            "200 OK".to_owned()
        } else {
            "421 Misdirected Request".to_owned()
        }
    } else {
        match path
            .strip_prefix("/status/")
            .and_then(|status| status.parse::<u16>().ok())
        {
            Some(status) => format!("{status} Status"),
            None => "200 OK".to_owned(),
        }
    };
    let response = format!("HTTP/1.1 {status}\r\nContent-Length: 3\r\nConnection: close\r\n\r\nok\n");
    let _ = stream.write_all(response.as_bytes());
    exit_code
}

/// The value of the `Host` header among the headers left in `reader`.
fn host(reader: &mut impl BufRead) -> Option<String> {
    let mut line = String::new();
    while reader.read_line(&mut line).ok()? > 0 && !line.trim().is_empty() {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("host")
        {
            return Some(value.trim().to_owned());
        }
        line.clear();
    }
    None
}
