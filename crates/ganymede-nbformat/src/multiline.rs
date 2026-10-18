//! Multi-line strings and Python-compatible line splitting.

use std::fmt;
use std::ops::Deref;

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::{Deserialize, Serialize, Serializer};

/// Split `s` into lines the way Python's `str.splitlines(keepends=True)` does.
///
/// Line boundaries are `\n`, `\r`, `\r\n` (one boundary), `\v`, `\f`,
/// `\x1c`, `\x1d`, `\x1e`, `\x85`, `\u{2028}` and `\u{2029}`. Each boundary
/// stays attached to the line it ends. An empty string yields no lines, and a
/// string without a trailing boundary yields a final line without one.
pub fn splitlines(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        let end = match c {
            '\r' => match iter.peek() {
                Some(&(j, '\n')) => {
                    iter.next();
                    j + 1
                }
                _ => i + 1,
            },
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => i + c.len_utf8(),
            _ => continue,
        };
        out.push(s[start..end].to_owned());
        start = end;
    }
    if start < s.len() {
        out.push(s[start..].to_owned());
    }
    out
}

/// A string that is stored on disk either as one JSON string or as an array
/// of line fragments. In memory it is always the joined string.
///
/// Deserializing accepts both forms (an array is concatenated, exactly like
/// nbformat's `rejoin_lines`), and reads `null` as the empty string.
/// Serializing produces a plain string; the notebook writer is what splits it
/// back into lines.
#[derive(Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MultilineString(String);

impl MultilineString {
    /// Wrap a string.
    pub fn new(s: impl Into<String>) -> Self {
        MultilineString(s.into())
    }

    /// The joined text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume into the joined text.
    pub fn into_string(self) -> String {
        self.0
    }

    /// Mutable access to the joined text.
    pub fn as_mut_string(&mut self) -> &mut String {
        &mut self.0
    }

    /// The text split into lines as nbformat writes it (see [`splitlines`]).
    pub fn lines(&self) -> Vec<String> {
        splitlines(&self.0)
    }

    /// Whether the text is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for MultilineString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for MultilineString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Deref for MultilineString {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for MultilineString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for MultilineString {
    fn from(s: String) -> Self {
        MultilineString(s)
    }
}

impl From<&str> for MultilineString {
    fn from(s: &str) -> Self {
        MultilineString(s.to_owned())
    }
}

impl From<Vec<String>> for MultilineString {
    fn from(lines: Vec<String>) -> Self {
        MultilineString(lines.concat())
    }
}

impl From<MultilineString> for String {
    fn from(m: MultilineString) -> String {
        m.0
    }
}

impl PartialEq<str> for MultilineString {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for MultilineString {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl Serialize for MultilineString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for MultilineString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;

        impl<'de> Visitor<'de> for V {
            type Value = MultilineString;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string, an array of strings, or null")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(MultilineString(v.to_owned()))
            }

            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(MultilineString(v))
            }

            /// `null` reads as empty: nbformat keeps an invalid `source:
            /// null`, and treating it as "" keeps the notebook usable.
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(MultilineString::default())
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(MultilineString::default())
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut joined = String::new();
                while let Some(part) = seq.next_element::<String>()? {
                    joined.push_str(&part);
                }
                Ok(MultilineString(joined))
            }
        }

        deserializer.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_matches_python() {
        // Each case: input, expected `input.splitlines(True)` from CPython.
        let cases: &[(&str, &[&str])] = &[
            ("", &[]),
            ("abc", &["abc"]),
            ("abc\n", &["abc\n"]),
            ("a\nb", &["a\n", "b"]),
            ("a\nb\n", &["a\n", "b\n"]),
            ("\n", &["\n"]),
            ("\n\n", &["\n", "\n"]),
            ("a\r\nb", &["a\r\n", "b"]),
            ("\r\n", &["\r\n"]),
            ("\n\r", &["\n", "\r"]),
            ("\r\r\n", &["\r", "\r\n"]),
            (
                "\rprogress: 0%\rprogress: 100%\n",
                &["\r", "progress: 0%\r", "progress: 100%\n"],
            ),
            (
                "a\x0bb\x0cc\x1cd\x1de\x1ef",
                &["a\x0b", "b\x0c", "c\x1c", "d\x1d", "e\x1e", "f"],
            ),
            (
                "a\u{85}b\u{2028}c\u{2029}d",
                &["a\u{85}", "b\u{2028}", "c\u{2029}", "d"],
            ),
            // Not boundaries: tab, NUL, other unicode spaces.
            ("a\tb\0c\u{a0}d\u{2029}", &["a\tb\0c\u{a0}d\u{2029}"]),
            ("héllo\n🦀\n", &["héllo\n", "🦀\n"]),
        ];
        for (input, expected) in cases {
            let got = splitlines(input);
            let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
            assert_eq!(got, expected, "splitlines({input:?})");
            // keepends=True means the pieces concatenate back to the input.
            assert_eq!(got.concat(), *input);
        }
    }

    #[test]
    fn deserializes_from_string_and_array() {
        let a: MultilineString = serde_json::from_str(r#""x\ny""#).unwrap();
        assert_eq!(a, "x\ny");
        let b: MultilineString = serde_json::from_str(r#"["x\n", "y"]"#).unwrap();
        assert_eq!(b, "x\ny");
        let c: MultilineString = serde_json::from_str("[]").unwrap();
        assert_eq!(c, "");
        assert!(serde_json::from_str::<MultilineString>("42").is_err());
        assert!(serde_json::from_str::<MultilineString>(r#"["a", 1]"#).is_err());
        let d: MultilineString = serde_json::from_str("null").unwrap();
        assert_eq!(d, "");
    }

    #[test]
    fn serializes_as_plain_string() {
        let m = MultilineString::from("x\ny\n");
        assert_eq!(serde_json::to_string(&m).unwrap(), r#""x\ny\n""#);
        assert_eq!(m.lines(), vec!["x\n".to_string(), "y\n".to_string()]);
        assert_eq!(m.to_string(), "x\ny\n");
        assert_eq!(
            MultilineString::from(vec!["a\n".to_string(), "b".to_string()]),
            "a\nb"
        );
    }

    #[test]
    fn works_inside_internally_tagged_enums() {
        // Internally tagged enums buffer content before dispatching, which
        // exercises the deserialize_any path through serde's Content type.
        #[derive(Deserialize)]
        #[serde(tag = "t")]
        enum E {
            A { s: MultilineString },
        }
        let E::A { s } = serde_json::from_str(r#"{"t": "A", "s": ["1\n", "2"]}"#).unwrap();
        assert_eq!(s, "1\n2");
    }
}
