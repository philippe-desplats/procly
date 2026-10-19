//! The brace placeholders of a template, `{name}`, shared by every text Procly renders: the command of a Service,
//! the URL and the commands of a Driver, and the Derived variables.
//!
//! Each caller decides which names are placeholders of its own and what happens to the others. A text is full of
//! braces that are not placeholders (`${VAR}`, `awk '{ print }'`, `find -exec {}`), so a brace that does not close
//! on a name is kept as it is, and a brace holding another brace opens nothing, the inner one being judged on its own.

/// A piece of a template, in order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Piece<'a> {
    Text(&'a str),
    /// A name between 2 braces, holding no brace itself, which may or may not be a placeholder of the caller.
    Braced(&'a str),
}

/// The pieces of `text`, see the module. Joined back, with braces around each [`Piece::Braced`], they give `text`.
pub(crate) fn pieces(text: &str) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let (mut literal, mut from) = (0, 0);
    while let Some(offset) = text[from..].find('{') {
        let open = from + offset;
        let after = &text[open + 1..];
        match after.find(['{', '}']) {
            Some(close) if after[close..].starts_with('}') => {
                if literal < open {
                    pieces.push(Piece::Text(&text[literal..open]));
                }
                pieces.push(Piece::Braced(&after[..close]));
                from = open + close + 2;
                literal = from;
            }
            _ => from = open + 1,
        }
    }
    if literal < text.len() {
        pieces.push(Piece::Text(&text[literal..]));
    }
    pieces
}

/// Renders `text`, replacing each braced name with what `value` gives for it. `value` answers `None` for a name that
/// is not a placeholder of the caller, which is then kept with its braces, or fails the whole rendering.
pub(crate) fn render<E>(text: &str, mut value: impl FnMut(&str) -> Result<Option<String>, E>) -> Result<String, E> {
    let mut rendered = String::with_capacity(text.len());
    for piece in pieces(text) {
        match piece {
            Piece::Text(text) => rendered.push_str(text),
            Piece::Braced(name) => {
                if let Some(value) = value(name)? {
                    rendered.push_str(&value);
                } else {
                    rendered.push('{');
                    rendered.push_str(name);
                    rendered.push('}');
                }
            }
        }
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_splits_into_literals_and_braced_names() {
        assert_eq!(
            pieces("npm run dev -- --port {ports.http} {}"),
            [
                Piece::Text("npm run dev -- --port "),
                Piece::Braced("ports.http"),
                Piece::Text(" "),
                Piece::Braced(""),
            ]
        );
    }

    #[test]
    fn a_brace_holding_another_or_never_closed_opens_nothing() {
        assert_eq!(
            pieces("${{ports.http}} {ports"),
            [Piece::Text("${"), Piece::Braced("ports.http"), Piece::Text("} {ports")]
        );
        assert_eq!(
            pieces("awk '{ print }'"),
            [Piece::Text("awk '"), Piece::Braced(" print "), Piece::Text("'")]
        );
    }

    #[test]
    fn a_name_the_caller_does_not_know_keeps_its_braces() {
        let rendered = render("{a} and {b}", |name| Ok::<_, ()>((name == "a").then(|| "1".to_owned())));
        assert_eq!(rendered, Ok("1 and {b}".to_owned()));
    }

    #[test]
    fn a_failing_name_fails_the_rendering() {
        assert_eq!(render("{a}", |name| Err(name.to_owned())), Err("a".to_owned()));
    }
}
