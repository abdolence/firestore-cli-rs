use console::AnsiCodeIterator;

/// `text` cut to at most `width` columns, keeping its start and its end around one `…`, since the
/// start and the end of a name are the parts that tell one name from another. When the remaining
/// columns are odd the start keeps the extra one. Columns are display widths, so a wide character
/// counts for two and a combining mark for none, and ANSI styling is carried through unmeasured.
/// A text that fits is returned as it is.
pub fn cut_middle(text: &str, width: usize) -> String {
    if console::measure_text_width(text) <= width {
        return text.to_string();
    }
    match width {
        0 => return String::new(),
        1 => return ELLIPSIS.to_string(),
        _ => {}
    }
    let parts: Vec<Part> = AnsiCodeIterator::new(text)
        .flat_map(|(segment, is_code)| {
            if is_code {
                vec![Part::Code(segment)]
            } else {
                segment.chars().map(Part::letter).collect()
            }
        })
        .collect();
    let kept = width - ELLIPSIS_WIDTH;
    let head_end = head_end(&parts, kept - kept / 2);
    let tail_start = tail_start(&parts, head_end, kept / 2);

    let mut out = String::new();
    let mut cut = false;
    for (position, part) in parts.iter().enumerate() {
        match part {
            Part::Code(code) => out.push_str(code),
            Part::Letter { letter, .. } if position < head_end || position >= tail_start => {
                out.push(*letter)
            }
            Part::Letter { .. } if !cut => {
                out.push_str(ELLIPSIS);
                cut = true;
            }
            Part::Letter { .. } => {}
        }
    }
    out
}

const ELLIPSIS: &str = "…";
const ELLIPSIS_WIDTH: usize = 1;

/// One piece of a text: an ANSI code, which has no width and is always kept, or a character.
enum Part<'a> {
    Code(&'a str),
    Letter { letter: char, width: usize },
}

impl Part<'_> {
    fn letter(letter: char) -> Self {
        Part::Letter {
            letter,
            width: console::measure_text_width(letter.encode_utf8(&mut [0; 4])),
        }
    }

    fn width(&self) -> Option<usize> {
        match self {
            Part::Code(_) => None,
            Part::Letter { width, .. } => Some(*width),
        }
    }
}

/// The position after the last part of the start that fits in `budget` columns. A character
/// without width, such as a combining mark, stays with the letter before it.
fn head_end(parts: &[Part], budget: usize) -> usize {
    let mut used = 0;
    for (position, part) in parts.iter().enumerate() {
        if let Some(width) = part.width() {
            if used + width > budget {
                return position;
            }
            used += width;
        }
    }
    parts.len()
}

/// The position of the first part of the end that fits in `budget` columns, never before
/// `head_end`. An end does not start with a character without width, whose letter was cut.
fn tail_start(parts: &[Part], head_end: usize, budget: usize) -> usize {
    let mut used = 0;
    let mut start = parts.len();
    for position in (head_end..parts.len()).rev() {
        if let Some(width) = parts[position].width() {
            if used + width > budget {
                break;
            }
            used += width;
            start = position;
        }
    }
    while parts[start..].first().and_then(Part::width) == Some(0) {
        start += 1;
    }
    start
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "pipeline-restaurants/rest-08";

    #[test]
    fn a_long_text_keeps_its_start_and_end_around_one_ellipsis() {
        assert_eq!(cut_middle(NAME, 18), "pipeline-…/rest-08");
        assert_eq!(cut_middle(NAME, 17), "pipeline…/rest-08");
        assert_eq!(cut_middle(NAME, 2), "p…");
        assert_eq!(cut_middle(NAME, 1), "…");
        assert_eq!(cut_middle(NAME, 0), "");
    }

    #[test]
    fn a_text_that_fits_is_returned_as_it_is() {
        assert_eq!(cut_middle(NAME, 28), NAME);
        assert_eq!(cut_middle(NAME, 80), NAME);
    }

    #[test]
    fn wide_characters_count_for_two_columns_and_a_combining_mark_stays_with_its_letter() {
        assert_eq!(cut_middle("日本語のテキスト", 9), "日本…スト");
        assert_eq!(cut_middle("日本語のテキスト", 8), "日本…ト");
        assert_eq!(
            cut_middle("abcde\u{301}xxxxxxxxxx", 11),
            "abcde\u{301}…xxxxx"
        );
    }

    #[test]
    fn styling_is_kept_around_the_cut_text_and_not_measured() {
        let styled = format!("\x1b[34m\x1b[4m{NAME}\x1b[0m");

        let cut = cut_middle(&styled, 18);

        assert_eq!(cut, "\x1b[34m\x1b[4mpipeline-…/rest-08\x1b[0m");
        assert_eq!(console::measure_text_width(&cut), 18);
    }
}
