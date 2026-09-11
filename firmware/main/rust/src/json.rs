//! A tiny, allocation-free JSON reader for the two response shapes the firmware
//! consumes.
//!
//! This is a *targeted scanner*, not a general parser: it finds the value of a
//! given key inside an object and skips over everything else. That is all
//! `page_sync` and `notify` need, and it avoids pulling a JSON DOM (and an
//! allocator) into the firmware. `bytes_at`/`str_value` deliberately reject
//! escaped or multi-byte content: the fields we read are hex ids, base64 blobs
//! and integers, so anything else means the server sent something unexpected
//! and we would rather fail than decode it wrong.

/// Index of the first byte after whitespace starting at `at`.
fn ws(s: &[u8], mut at: usize) -> usize {
    while at < s.len() && matches!(s[at], b' ' | b'\t' | b'\r' | b'\n') {
        at += 1;
    }
    at
}

fn byte(s: &[u8], at: usize) -> Option<u8> {
    s.get(at).copied()
}

/// Index just past the string starting at `at` (which must be `"`).
///
/// Escape-aware: `\X` consumes two bytes so a `\"` inside a string cannot fool
/// the scanner into ending it early.
fn skip_string(s: &[u8], at: usize) -> Option<usize> {
    if byte(s, at)? != b'"' {
        return None;
    }
    let mut i = at + 1;
    while i < s.len() {
        match s[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Index just past the value starting at `at`.
pub fn skip_value(s: &[u8], at: usize) -> Option<usize> {
    let i = ws(s, at);
    match byte(s, i)? {
        b'"' => skip_string(s, i),
        b'{' => {
            let mut j = ws(s, i + 1);
            if byte(s, j) == Some(b'}') {
                return Some(j + 1);
            }
            loop {
                j = skip_string(s, j)?;
                j = ws(s, j);
                if byte(s, j)? != b':' {
                    return None;
                }
                j = skip_value(s, j + 1)?;
                j = ws(s, j);
                match byte(s, j)? {
                    b',' => j = ws(s, j + 1),
                    b'}' => return Some(j + 1),
                    _ => return None,
                }
            }
        }
        b'[' => {
            let mut j = ws(s, i + 1);
            if byte(s, j) == Some(b']') {
                return Some(j + 1);
            }
            loop {
                j = skip_value(s, j)?;
                j = ws(s, j);
                match byte(s, j)? {
                    b',' => j = ws(s, j + 1),
                    b']' => return Some(j + 1),
                    _ => return None,
                }
            }
        }
        b't' => s.get(i..i + 4).filter(|v| v == b"true").map(|_| i + 4),
        b'f' => s.get(i..i + 5).filter(|v| v == b"false").map(|_| i + 5),
        b'n' => s.get(i..i + 4).filter(|v| v == b"null").map(|_| i + 4),
        _ => {
            let mut j = i;
            while j < s.len() && matches!(s[j], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                j += 1;
            }
            (j > i).then_some(j)
        }
    }
}

/// Index of the value bound to `key` in the object starting at `obj`.
///
/// Only this object's own members are searched; nested objects are skipped
/// whole, so a key that also appears deeper cannot shadow the outer one.
pub fn member(s: &[u8], obj: usize, key: &str) -> Option<usize> {
    let key = key.as_bytes();
    let mut i = obj;
    if byte(s, i)? != b'{' {
        return None;
    }
    i = ws(s, i + 1);
    if byte(s, i) == Some(b'}') {
        return None;
    }
    loop {
        let name_start = i + 1;
        let name_end = skip_string(s, i)? - 1;
        let colon = ws(s, name_end + 1);
        if byte(s, colon)? != b':' {
            return None;
        }
        let value = ws(s, colon + 1);
        if s.get(name_start..name_end) == Some(key) {
            return Some(value);
        }
        let after = ws(s, skip_value(s, value)?);
        match byte(s, after)? {
            b',' => i = ws(s, after + 1),
            b'}' => return None,
            _ => return None,
        }
    }
}

/// `member` applied along a path, e.g. `["notification", "id"]`.
pub fn path(s: &[u8], keys: &[&str]) -> Option<usize> {
    let (first, rest) = keys.split_first()?;
    let mut at = member(s, 0, first)?;
    for key in rest {
        at = member(s, at, key)?;
    }
    Some(at)
}

/// Content of the string at `at`, if it is a string without escapes.
///
/// Rejecting escapes is intentional: every field the firmware reads is hex or
/// base64, so a backslash means the response is not what we expect.
pub fn str_value(s: &[u8], at: usize) -> Option<&[u8]> {
    let end = skip_string(s, at)?;
    let content = s.get(at + 1..end - 1)?;
    (!content.contains(&b'\\')).then_some(content)
}

/// Integer value at `at` (no fractions, no exponent).
pub fn int_value(s: &[u8], at: usize) -> Option<i64> {
    let end = skip_value(s, at)?;
    let raw = s.get(at..end)?;
    let text = core::str::from_utf8(raw).ok()?;
    text.parse::<i64>().ok()
}

/// Call `f(item_start)` for each element of the array at `at`.
///
/// `f` returns `false` to stop early. Returns the number of items visited, or
/// `None` if `at` is not an array.
pub fn for_each_item(s: &[u8], at: usize, f: &mut impl FnMut(usize) -> bool) -> Option<usize> {
    if byte(s, at)? != b'[' {
        return None;
    }
    let mut i = ws(s, at + 1);
    if byte(s, i) == Some(b']') {
        return Some(0);
    }
    let mut count = 0;
    loop {
        let start = ws(s, i);
        count += 1;
        if !f(start) {
            return Some(count);
        }
        let after = ws(s, skip_value(s, start)?);
        match byte(s, after)? {
            b',' => i = after + 1,
            b']' => return Some(count),
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEXT: &[u8] =
        br#"{"bitmap_base64":"AAECAw==","notification":{"id":"deadbeef","title":"t, } {\"","status":"shown"}}"#;

    #[test]
    fn finds_flat_and_nested_members() {
        assert_eq!(str_value(NEXT, member(NEXT, 0, "bitmap_base64").unwrap()),
                   Some(&b"AAECAw=="[..]));
        assert_eq!(str_value(NEXT, path(NEXT, &["notification", "id"]).unwrap()),
                   Some(&b"deadbeef"[..]));
        assert_eq!(str_value(NEXT, path(NEXT, &["notification", "status"]).unwrap()),
                   Some(&b"shown"[..]));
    }

    #[test]
    fn escaped_braces_inside_a_string_do_not_confuse_the_scanner() {
        // The title contains `,`, `}`, `{` and an escaped quote; the member
        // after it must still be found.
        let at = path(NEXT, &["notification", "status"]).unwrap();
        assert_eq!(str_value(NEXT, at), Some(&b"shown"[..]));
    }

    #[test]
    fn nested_keys_do_not_shadow_outer_ones() {
        let s = br#"{"id":"outer","inner":{"id":"inner"}}"#;
        assert_eq!(str_value(s, member(s, 0, "id").unwrap()), Some(&b"outer"[..]));
    }

    #[test]
    fn iterates_an_array_of_objects() {
        let s = br#"{"pages":[{"md5":"aa","order":0},{"md5":"bb","order":1}]}"#;
        let arr = member(s, 0, "pages").unwrap();
        let mut seen = Vec::new();
        let n = for_each_item(s, arr, &mut |item| {
            seen.push(str_value(s, member(s, item, "md5").unwrap()).unwrap().to_vec());
            true
        });
        assert_eq!(n, Some(2));
        assert_eq!(seen, vec![b"aa".to_vec(), b"bb".to_vec()]);
    }

    #[test]
    fn reads_integers() {
        let s = br#"{"duration_minutes":10,"order":-2,"ratio":1.5}"#;
        assert_eq!(int_value(s, member(s, 0, "duration_minutes").unwrap()), Some(10));
        assert_eq!(int_value(s, member(s, 0, "order").unwrap()), Some(-2));
        assert_eq!(int_value(s, member(s, 0, "ratio").unwrap()), None);
    }

    #[test]
    fn missing_or_truncated_input_is_none() {
        let s = br#"{"a":1}"#;
        assert_eq!(member(s, 0, "b"), None);
        assert_eq!(member(&s[..4], 0, "a"), None);
        assert_eq!(str_value(s, member(s, 0, "a").unwrap()), None); // not a string
        assert_eq!(path(s, &["a", "b"]), None);
        assert_eq!(skip_value(b"", 0), None);
    }

    #[test]
    fn empty_containers() {
        let s = br#"{"o":{},"a":[],"pages":[]}"#;
        assert_eq!(for_each_item(s, member(s, 0, "pages").unwrap(), &mut |_| true), Some(0));
        assert_eq!(member(s, member(s, 0, "o").unwrap(), "x"), None);
    }

    #[test]
    fn escapes_are_skipped_but_rejected_as_values() {
        let s = br#"{"t":"a\"b","v":"plain"}"#;
        // The escaped string is skipped correctly, so `v` is still found.
        assert_eq!(str_value(s, member(s, 0, "v").unwrap()), Some(&b"plain"[..]));
        assert_eq!(str_value(s, member(s, 0, "t").unwrap()), None);
    }
}
