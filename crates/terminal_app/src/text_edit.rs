//! UTF-16 text-editing primitives shared by the app's text fields.
//!
//! GPUI's input protocol addresses text by UTF-16 offset, while Rust strings are
//! UTF-8 indexed by byte. The settings page's inline fields and the terminal
//! search bar both need the same conversions, so they live here instead of in
//! either caller.

use std::ops::Range;

pub(crate) fn previous_utf16_boundary(text: &str, offset: usize) -> usize {
    let mut previous = 0;
    let mut current = 0;
    for character in text.chars() {
        let next = current + character.len_utf16();
        if offset <= current {
            return previous;
        }
        if offset <= next {
            return current;
        }
        previous = current;
        current = next;
    }
    current
}

pub(crate) fn next_utf16_boundary(text: &str, offset: usize) -> usize {
    let mut current = 0;
    for character in text.chars() {
        let next = current + character.len_utf16();
        if offset < next {
            return next;
        }
        current = next;
    }
    current
}

fn byte_offset_for_utf16(text: &str, utf16_offset: usize) -> usize {
    let mut consumed_utf16 = 0;
    for (byte_offset, character) in text.char_indices() {
        if consumed_utf16 >= utf16_offset {
            return byte_offset;
        }
        let next = consumed_utf16 + character.len_utf16();
        if utf16_offset < next {
            return byte_offset;
        }
        consumed_utf16 = next;
    }
    text.len()
}

pub(crate) fn substring_utf16(text: &str, range: Range<usize>) -> String {
    let start = byte_offset_for_utf16(text, range.start);
    let end = byte_offset_for_utf16(text, range.end.max(range.start));
    text[start..end].to_string()
}

pub(crate) fn replace_utf16_range(
    text: &str,
    range: Range<usize>,
    replacement: &str,
) -> (String, usize) {
    let start_utf16 = range.start.min(text.encode_utf16().count());
    let end_utf16 = range.end.max(start_utf16).min(text.encode_utf16().count());
    let start = byte_offset_for_utf16(text, start_utf16);
    let end = byte_offset_for_utf16(text, end_utf16);
    let mut updated = String::with_capacity(text.len() + replacement.len());
    updated.push_str(&text[..start]);
    updated.push_str(replacement);
    updated.push_str(&text[end..]);
    (updated, start_utf16 + replacement.encode_utf16().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_and_replacement_are_utf16_aware() {
        // A multi-byte, multi-UTF-16 scalar sits between the ASCII characters.
        let text = "a\u{1F600}b";
        assert_eq!(text.encode_utf16().count(), 4);

        assert_eq!(previous_utf16_boundary(text, 0), 0);
        assert_eq!(previous_utf16_boundary(text, 1), 0);
        assert_eq!(previous_utf16_boundary(text, 3), 1);
        assert_eq!(next_utf16_boundary(text, 0), 1);
        assert_eq!(next_utf16_boundary(text, 1), 3);
        assert_eq!(next_utf16_boundary(text, 3), 4);

        assert_eq!(substring_utf16(text, 1..3), "\u{1F600}");
        assert_eq!(substring_utf16(text, 0..4), text);
        assert_eq!(replace_utf16_range(text, 1..3, "x"), ("axb".to_string(), 2));
    }
}
