//! Reading a shell command line, which the Guard and init both do: the splitting of a line into simple commands and
//! words, the words that only launch a command, and the one normalisation of the paths a command is given.
//!
//! The reading is lexical. Nothing here runs a shell or looks at the filesystem, and quotes, escapes and the separators
//! of a list are the whole grammar.

use std::ops::Range;
use std::path::{Component, Path, PathBuf};

/// A simple command of a command line: its text as typed, where that text sits in the command line, and its words with
/// quotes and escapes resolved, each with where it sits too.
#[derive(Debug)]
pub(crate) struct Segment {
    pub(crate) text: String,
    pub(crate) span: Range<usize>,
    pub(crate) words: Vec<String>,
    pub(crate) word_spans: Vec<Range<usize>>,
}

/// Splits a command line into simple commands at semicolons, `&`, `|`, parentheses, backticks and line breaks, and each
/// command into words, the way a shell would outside of quotes. A separator inside quotes splits nothing, so
/// `echo "a && npm run dev"` is one command that starts nothing.
pub(crate) fn segments(command_line: &str) -> Vec<Segment> {
    #[derive(Default)]
    struct Reading {
        segments: Vec<Segment>,
        words: Vec<String>,
        word_spans: Vec<Range<usize>>,
        word: Option<String>,
        word_start: usize,
        start: usize,
    }
    impl Reading {
        /// The word being read, started at `at` if there is none.
        fn current(&mut self, at: usize) -> &mut String {
            self.word.get_or_insert_with(|| {
                self.word_start = at;
                String::new()
            })
        }
        /// Ends the word being read, which stops at `end`.
        fn end_word(&mut self, end: usize) {
            if let Some(word) = self.word.take() {
                self.words.push(word);
                self.word_spans.push(self.word_start..end);
            }
        }
        fn end_segment(&mut self, end: usize, text: &str) {
            self.end_word(end);
            if !self.words.is_empty() {
                let raw = &text[self.start..end];
                let start = self.start + (raw.len() - raw.trim_start().len());
                let trimmed = raw.trim();
                self.segments.push(Segment {
                    text: trimmed.to_owned(),
                    span: start..start + trimmed.len(),
                    words: std::mem::take(&mut self.words),
                    word_spans: std::mem::take(&mut self.word_spans),
                });
            }
        }
    }

    let mut reading = Reading::default();
    let mut quote: Option<char> = None;
    let mut chars = command_line.char_indices();
    while let Some((at, c)) = chars.next() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some('"'), '\\') => {
                if let Some((_, escaped)) = chars.next() {
                    reading.current(at).push(escaped);
                }
            }
            (None, '\'' | '"') => {
                quote = Some(c);
                reading.current(at);
            }
            (None, '\\') => match chars.next() {
                Some((_, '\n')) | None => {}
                Some((_, escaped)) => reading.current(at).push(escaped),
            },
            (None, ' ' | '\t') => reading.end_word(at),
            (None, '\n' | ';' | '&' | '|' | '(' | ')' | '`') => {
                reading.end_segment(at, command_line);
                reading.start = at + c.len_utf8();
            }
            (_, c) => reading.current(at).push(c),
        }
    }
    reading.end_segment(command_line.len(), command_line);
    reading.segments
}

/// The last component of a program path: `./node_modules/.bin/next` is `next`.
pub(crate) fn basename(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// Whether a word is a `NAME=value` assignment, which a shell applies to the command that follows it.
pub(crate) fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The words of a command without what only launches it: variables set for it, `nohup`, `exec`, `time`, `npx`,
/// `pnpm exec`, `bundle exec` and the like.
pub(crate) fn strip_launchers(mut words: &[String]) -> &[String] {
    loop {
        let Some(first) = words.first() else {
            return words;
        };
        let second = words.get(1).map(String::as_str);
        let skip = match (basename(first), second) {
            _ if is_assignment(first) => 1,
            // `command -v vite` asks where a program is and starts nothing.
            ("command", Some("-v" | "-V")) => return words,
            // Also the keywords that open a command in a shell script: `if ready; then npm run dev; fi`.
            (
                "env" | "nohup" | "exec" | "time" | "command" | "setsid" | "npx" | "bunx" | "pnpx" | "then" | "do"
                | "else" | "!" | "{",
                _,
            ) => 1,
            ("pnpm" | "yarn", Some("exec" | "dlx"))
            | ("bun", Some("x"))
            | ("npm" | "bundle", Some("exec"))
            | ("poetry" | "uv", Some("run")) => 2,
            _ => return words,
        };
        words = &words[skip..];
        // The options of the wrapper, such as `npx -y`.
        while words.first().is_some_and(|word| word.starts_with('-')) {
            words = &words[1..];
        }
    }
}

/// The words after the program that are not options, skipping the value of an option that takes one.
pub(crate) fn positional<'a>(words: &'a [String], with_value: &[&str]) -> Vec<&'a str> {
    let mut positional = Vec::new();
    let mut words = words.iter().map(String::as_str);
    while let Some(word) = words.next() {
        if with_value.contains(&word) {
            words.next();
        } else if !word.starts_with('-') {
            positional.push(word);
        }
    }
    positional
}

/// A path with its `.` and `..` resolved, and where it ends up against the base it was read from.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// A path that stays inside its base, relative to it.
    Inside(PathBuf),
    /// A path that climbs above its base, relative to it, so it keeps its leading `..`.
    Above(PathBuf),
    /// A path that starts at `/`, which says nothing about where the base is. A `..` at the root stays at the root.
    Absolute(PathBuf),
}

/// `path` as written from `base`, with its `.` and `..` resolved without looking at the filesystem. `base` is a
/// relative path, and `path` replaces it when it starts at `/`.
pub(crate) fn resolve(base: &Path, path: &str) -> Resolved {
    let mut resolved = PathBuf::new();
    let mut above = false;
    for component in base.join(path).components() {
        match component {
            Component::Normal(name) => resolved.push(name),
            Component::CurDir => {}
            Component::ParentDir if resolved.file_name().is_some() => {
                resolved.pop();
            }
            Component::ParentDir if resolved.has_root() => {}
            Component::ParentDir => {
                resolved.push("..");
                above = true;
            }
            root @ (Component::RootDir | Component::Prefix(_)) => resolved.push(root),
        }
    }
    match (resolved.has_root(), above) {
        (true, _) => Resolved::Absolute(resolved),
        (false, true) => Resolved::Above(resolved),
        (false, false) => Resolved::Inside(resolved),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(command: &str) -> Vec<String> {
        command.split(' ').map(str::to_owned).collect()
    }

    fn texts(command_line: &str) -> Vec<String> {
        segments(command_line).into_iter().map(|segment| segment.text).collect()
    }

    #[test]
    fn a_command_line_is_split_at_every_separator_outside_of_quotes() {
        assert_eq!(texts("cd web && npm run dev"), ["cd web", "npm run dev"]);
        assert_eq!(texts("a; b | c\nd (e) `f`"), ["a", "b", "c", "d", "e", "f"]);
        assert_eq!(texts("echo \"a && npm run dev\""), ["echo \"a && npm run dev\""]);
        assert_eq!(texts("echo 'a; b'"), ["echo 'a; b'"]);
        assert!(segments("").is_empty());
        assert!(segments("  ; & ").is_empty());
    }

    #[test]
    fn a_segment_holds_its_words_with_quotes_and_escapes_resolved_and_where_they_sit() {
        let command_line = "  FOO=1 sh -c \"a b\" c\\ d";
        let [segment] = segments(command_line).try_into().expect("one segment");
        assert_eq!(segment.words, ["FOO=1", "sh", "-c", "a b", "c d"]);
        assert_eq!(segment.text, "FOO=1 sh -c \"a b\" c\\ d");
        assert_eq!(&command_line[segment.span.clone()], segment.text);
        assert_eq!(&command_line[segment.word_spans[3].clone()], "\"a b\"");
        assert_eq!(&command_line[segment.word_spans[4].clone()], "c\\ d");
    }

    #[test]
    fn a_line_continuation_joins_the_lines_and_a_line_break_splits_them() {
        assert_eq!(segments("npm \\\nrun dev")[0].words, ["npm", "run", "dev"]);
        assert_eq!(texts("npm run build\nnpm run dev"), ["npm run build", "npm run dev"]);
    }

    #[test]
    fn a_redirection_to_a_stream_is_split_at_its_ampersand() {
        // The Guard reads `2>&1` this way, and init glues it back where it needs it.
        assert_eq!(
            texts("nohup npm run dev > dev.log 2>&1 &"),
            ["nohup npm run dev > dev.log 2>", "1"]
        );
        assert_eq!(texts("next dev &>dev.log"), ["next dev", ">dev.log"]);
    }

    #[test]
    fn the_basename_of_a_program_is_its_last_component() {
        assert_eq!(basename("./node_modules/.bin/next"), "next");
        assert_eq!(basename("next"), "next");
        assert_eq!(basename("bin/"), "");
    }

    #[test]
    fn only_a_name_equals_a_value_is_an_assignment() {
        for word in ["PORT=3000", "_x=", "a1=b=c"] {
            assert!(is_assignment(word), "{word}");
        }
        for word in ["PORT", "=3000", "1A=b", "A-B=c", "--port=3000"] {
            assert!(!is_assignment(word), "{word}");
        }
    }

    #[test]
    fn what_only_launches_a_command_is_stripped_from_its_words() {
        for (command, expected) in [
            ("PORT=3000 npm run dev", "npm run dev"),
            ("FOO=1 BAR=2 env nohup next dev", "next dev"),
            ("npx -y vite", "vite"),
            ("pnpm exec next dev", "next dev"),
            ("bunx --bun astro dev", "astro dev"),
            ("bundle exec rails s", "rails s"),
            ("then npm run dev", "npm run dev"),
            ("command -v vite", "command -v vite"),
            ("npm run dev", "npm run dev"),
        ] {
            let all = words(command);
            assert_eq!(strip_launchers(&all), words(expected), "{command}");
        }
        assert_eq!(strip_launchers(&[]), Vec::<String>::new());
    }

    #[test]
    fn the_positional_words_skip_options_and_the_values_of_those_that_take_one() {
        let all = words("--filter web run dev --silent");
        assert_eq!(positional(&all, &["--filter"]), ["run", "dev"]);
        assert_eq!(positional(&all, &[]), ["web", "run", "dev"]);
    }

    #[test]
    fn a_path_is_resolved_against_its_base_and_stays_inside_it() {
        for (base, path, expected) in [
            ("", "", ""),
            ("", ".", ""),
            ("", "apps/web", "apps/web"),
            ("apps", "./web", "apps/web"),
            ("apps/web", "../api", "apps/api"),
            ("apps/web", "../..", ""),
            ("apps", "web/../api/./src", "apps/api/src"),
        ] {
            assert_eq!(
                resolve(Path::new(base), path),
                Resolved::Inside(PathBuf::from(expected)),
                "{base} + {path}"
            );
        }
    }

    #[test]
    fn a_path_that_climbs_above_its_base_keeps_its_leading_parent_directories() {
        for (base, path, expected) in [
            ("", "..", ".."),
            ("", "../x", "../x"),
            ("apps", "../../..", "../.."),
            ("apps/web", "../../../x/../y", "../y"),
        ] {
            assert_eq!(
                resolve(Path::new(base), path),
                Resolved::Above(PathBuf::from(expected)),
                "{base} + {path}"
            );
        }
    }

    #[test]
    fn an_absolute_path_replaces_the_base_and_cannot_climb_above_the_root() {
        for (base, path, expected) in [
            ("apps", "/", "/"),
            ("", "/p/work/acme/web", "/p/work/acme/web"),
            ("apps", "/p/./work/../acme", "/p/acme"),
            ("", "/..", "/"),
            ("", "/../../p", "/p"),
        ] {
            assert_eq!(
                resolve(Path::new(base), path),
                Resolved::Absolute(PathBuf::from(expected)),
                "{base} + {path}"
            );
        }
    }
}
