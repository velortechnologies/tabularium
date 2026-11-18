//! UTF-8 byte windows with exact source offsets; no generated quotations.

#[derive(Debug, Clone)]
pub(super) struct TextChunk {
    pub start: usize,
    pub end: usize,
    pub line_start: usize,
    pub line_end: usize,
}

pub(super) fn split(body: &str, size: usize, overlap: usize) -> Vec<TextChunk> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut line_start = 1;
    while start < body.len() {
        let mut end = (start + size).min(body.len());
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        if end < body.len() {
            // Search on character boundaries, including for non-ASCII documents.
            let half = start + (end - start) / 2;
            let half = (half..=end)
                .find(|&i| body.is_char_boundary(i))
                .unwrap_or(end);
            if let Some(i) = body[half..end].rfind('\n') {
                end = half + i + 1;
            }
        }
        if end <= start {
            break;
        }
        if !body[start..end].trim().is_empty() {
            let line_end = line_start
                + body[start..end]
                    .trim_end_matches('\n')
                    .bytes()
                    .filter(|&b| b == b'\n')
                    .count();
            out.push(TextChunk {
                start,
                end,
                line_start,
                line_end,
            });
        }
        if end == body.len() {
            break;
        }
        let mut next = end.saturating_sub(overlap).max(start + 1);
        while next < end && !body.is_char_boundary(next) {
            next += 1;
        }
        // Count each prefix byte once, not the whole document prefix per chunk.
        line_start += body[start..next].bytes().filter(|&b| b == b'\n').count();
        start = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_offsets_cover_source_and_lines() {
        let s = "перший рядок\nдругий рядок\n".repeat(20);
        let chunks = split(&s, 37, 6);
        assert_eq!(chunks.first().unwrap().start, 0);
        assert_eq!(chunks.last().unwrap().end, s.len());
        for c in &chunks {
            assert!(s.get(c.start..c.end).is_some());
            assert!(c.end - c.start <= 37);
            assert_eq!(
                c.line_start,
                1 + s[..c.start].bytes().filter(|&b| b == b'\n').count()
            );
        }
        for w in chunks.windows(2) {
            assert!(w[1].start <= w[0].end);
            assert!(w[1].start > w[0].start);
        }
    }
    #[test]
    fn incremental_line_numbers_match_prefix_reference() {
        for text in [
            "abc\n\n xyz\n",
            "пташка 🖤\nперевірка\n",
            "   \n\n\n",
            "a\nb\nc\nd\n",
            "single line",
        ] {
            let body = text.repeat(100);
            for size in [16, 37, 64, 768] {
                for overlap in [0, 3, size / 2, size - 1] {
                    let chunks = split(&body, size, overlap);
                    for c in &chunks {
                        let expected_start =
                            1 + body[..c.start].bytes().filter(|&b| b == b'\n').count();
                        let expected_end = expected_start
                            + body[c.start..c.end]
                                .trim_end_matches('\n')
                                .bytes()
                                .filter(|&b| b == b'\n')
                                .count();
                        assert_eq!((c.line_start, c.line_end), (expected_start, expected_end));
                        assert!(body.get(c.start..c.end).is_some());
                    }
                    for pair in chunks.windows(2) {
                        assert!(pair[1].start > pair[0].start);
                    }
                }
            }
        }
    }
    #[test]
    fn large_document_chunking() {
        let body = "IEEE specification line with unicode: перевірка.\n".repeat(500_000);
        let at = std::time::Instant::now();
        let chunks = split(&body, 768, 96);
        eprintln!(
            "{} bytes -> {} chunks in {:?}",
            body.len(),
            chunks.len(),
            at.elapsed()
        );
        assert_eq!(chunks.last().unwrap().end, body.len());
        assert_eq!(chunks.last().unwrap().line_end, 500_000);
    }
}
