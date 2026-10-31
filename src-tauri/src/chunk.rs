//! Text cleanup and chunking. Standard library only, so it is easy to test in isolation.

/// Normalise extracted text before embedding:
/// - strips `\r` and trailing whitespace,
/// - turns control characters (form feeds from PDFs, etc.) into spaces,
/// - collapses runs of interior spaces/tabs (PDF layout output is full of them)
///   while keeping leading indentation,
/// - collapses any run of blank lines into a single blank line.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0usize;

    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run == 1 {
                out.push('\n');
            }
            continue;
        }
        blank_run = 0;

        let indent_len = line.len() - line.trim_start().len();
        out.push_str(&line[..indent_len]);
        let mut prev_space = false;
        for c in line[indent_len..].chars() {
            if c == ' ' || c == '\t' || c.is_control() {
                if !prev_space {
                    out.push(' ');
                }
                prev_space = true;
            } else {
                out.push(c);
                prev_space = false;
            }
        }
        out.push('\n');
    }

    out.trim().to_string()
}

/// Split `text` into overlapping chunks of at most `max_len` bytes.
///
/// Each cut is made at the most natural break found in the last 40% of the window
/// (paragraph > line > sentence > word), so chunks rarely end mid-word.
/// Consecutive chunks share roughly `overlap` bytes so an idea that straddles a
/// boundary is still retrievable from either side.
pub fn chunk(text: &str, max_len: usize, overlap: usize) -> Vec<String> {
    let max_len = max_len.max(64);
    let overlap = overlap.min(max_len / 2);
    let text = text.trim();
    let mut chunks = Vec::new();
    if text.is_empty() {
        return chunks;
    }

    let len = text.len();
    let mut start = 0usize;
    loop {
        if len - start <= max_len {
            push_trimmed(&mut chunks, &text[start..]);
            break;
        }

        let hard_end = floor_boundary(text, start + max_len);
        let min_end = floor_boundary(text, start + max_len * 6 / 10);
        let end = best_break(&text[min_end..hard_end])
            .map(|i| min_end + i)
            .filter(|&e| e > start)
            .unwrap_or(hard_end);

        push_trimmed(&mut chunks, &text[start..end]);

        // Step back by `overlap`, then forward to the next word start so the
        // overlapping chunk doesn't begin with half a word.
        let mut next = floor_boundary(text, end.saturating_sub(overlap));
        if next <= start {
            next = end;
        } else if let Some(ws) = text[next..end].find(char::is_whitespace) {
            next += ws;
            next += text[next..].chars().next().map_or(0, char::len_utf8);
        }
        start = next;
    }

    chunks
}

fn push_trimmed(chunks: &mut Vec<String>, piece: &str) {
    let piece = piece.trim();
    if !piece.is_empty() {
        chunks.push(piece.to_string());
    }
}

/// Byte offset (within `window`) just after the best place to cut.
fn best_break(window: &str) -> Option<usize> {
    if let Some(i) = window.rfind("\n\n") {
        return Some(i + 2);
    }
    if let Some(i) = window.rfind('\n') {
        return Some(i + 1);
    }
    if let Some(i) = window.rfind(". ") {
        return Some(i + 2);
    }
    window.rfind(' ').map(|i| i + 1)
}

/// Largest char boundary <= `i`.
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(chunk("hello world", 100, 20), vec!["hello world"]);
        assert!(chunk("   \n  ", 100, 20).is_empty());
    }

    #[test]
    fn chunks_respect_max_len_and_cover_text() {
        let para = "The quick brown fox jumps over the lazy dog. ".repeat(40);
        let text = format!("{para}\n\n{para}\n{para}");
        let chunks = chunk(&text, 300, 60);
        assert!(chunks.len() > 5);
        for c in &chunks {
            assert!(c.len() <= 300, "chunk too long: {}", c.len());
            assert!(!c.starts_with(' '));
        }
        // every word of the source appears in some chunk
        assert!(chunks.last().unwrap().ends_with("dog."));
    }

    #[test]
    fn multibyte_text_never_panics() {
        let text = "héllo wörld — ünïcødé 日本語のテキスト ".repeat(200);
        for (max, ov) in [(64, 10), (100, 50), (333, 7)] {
            let chunks = chunk(&text, max, ov);
            assert!(!chunks.is_empty());
            for c in &chunks {
                assert!(c.len() <= max.max(64));
            }
        }
    }

    #[test]
    fn text_without_spaces_still_progresses() {
        let text = "x".repeat(5000);
        let chunks = chunk(&text, 1000, 200);
        assert!(chunks.len() >= 5 && chunks.len() <= 8);
    }

    #[test]
    fn clean_collapses_whitespace() {
        let raw = "Title\r\n\r\n\r\n\r\nSome    spaced\ttext\x0c here   \n    indented   line\n";
        assert_eq!(clean(raw), "Title\n\nSome spaced text here\n    indented line");
    }
}
