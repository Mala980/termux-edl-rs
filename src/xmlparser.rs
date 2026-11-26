//! Minimal XML response parser for firehose `<response .../>` / `<log value="..."/>` chunks —
//! port of xmlparser.py (splits on `<?xml`, strips the magic junk token the devices emit).
pub fn split_xml_chunks(input: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(idx) = find(&input[pos..], b"<?xml") {
        let start = pos + idx;
        let rest = &input[start + 5..];
        let end = find(rest, b"<?xml").map(|i| start + 5 + i).unwrap_or(input.len());
        out.push(&input[start..end]);
        pos = end;
    }
    out
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn strip_junk(line: &[u8]) -> Vec<u8> {
    const MAGIC: [u8; 4] = [0xf0, 0xe9, 0x88, 0x14];
    let mut out = Vec::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        if i + 4 <= line.len() && line[i..i + 4] == MAGIC {
            i += 4;
        } else {
            out.push(line[i]);
            i += 1;
        }
    }
    out
}

/// Extract a single attribute set from a `<response .../>` element.
pub fn getresponse(input: &[u8]) -> Vec<(String, String)> {
    let mut content = Vec::new();
    for chunk in split_xml_chunks(input) {
        let line = strip_junk(chunk);
        if let Some(attrs) = parse_element(&line, b"response") {
            content = attrs;
        }
    }
    content
}

/// Extract all `<log value="...">` values.
pub fn getlog(input: &[u8]) -> Vec<String> {
    let mut data = Vec::new();
    for chunk in split_xml_chunks(input) {
        let line = strip_junk(chunk);
        if let Some(attrs) = parse_element(&line, b"log") {
            if let Some(v) = attrs.into_iter().find(|(k, _)| k == "value") {
                data.push(v.1);
            }
        }
    }
    data
}

/// Very small tolerant tag parser: finds `<tag ...attribs... />` or `<tag ...>...</tag>`.
fn parse_element(line: &[u8], tag: &[u8]) -> Option<Vec<(String, String)>> {
    // Find the actual element opening: "<tag" not preceded by '/' or '?'.
    let pat: Vec<u8> = [b"<", &tag[..]].concat();
    let mut start = None;
    let mut pos = 0usize;
    while let Some(idx) = find(&line[pos..], &pat) {
        let at = pos + idx;
        let next = *line.get(at + pat.len()).unwrap_or(&b'>');
        // must be a fresh tag: whitespace, '/', '>' after the name
        if next.is_ascii_whitespace() || next == b'/' || next == b'>' {
            start = Some(at);
            break;
        }
        pos = at + 1;
    }
    let start = start?;
    let name_end = start + pat.len();
    let end_rel = find(&line[name_end..], b">")?;
    let body = &line[name_end..name_end + end_rel];
    if body.first() == Some(&b'/') {
        return None;
    }
    let mut attrs = Vec::new();
    let mut p = 0usize;
    while p < body.len() {
        while p < body.len() && (body[p] as char).is_whitespace() {
            p += 1;
        }
        let key_start = p;
        while p < body.len() && body[p] != b'=' {
            p += 1;
        }
        if p >= body.len() {
            break;
        }
        let key = String::from_utf8_lossy(&body[key_start..p]).trim().to_string();
        p += 1;
        if p < body.len() && (body[p] == b'"' || body[p] == b'\'') {
            let quote = body[p];
            p += 1;
            let val_start = p;
            while p < body.len() && body[p] != quote {
                p += 1;
            }
            let val = String::from_utf8_lossy(&body[val_start..p]).to_string();
            p += 1;
            if !key.is_empty() {
                attrs.push((key, val));
            }
        }
    }
    Some(attrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_getresponse() {
        let input = b"junk<?xml version=\"1.0\"?><response value=\"ACK\" rawmode=\"false\" /><?xml version=\"1.0\"?><log value=\"hi\"/>";
        let resp = getresponse(input);
        assert_eq!(resp.iter().find(|(k, _)| k == "value").unwrap().1, "ACK");
        assert_eq!(resp.iter().find(|(k, _)| k == "rawmode").unwrap().1, "false");
        let logs = getlog(input);
        assert_eq!(logs, vec!["hi".to_string()]);
    }

    #[test]
    fn test_magic_stripped() {
        let input = b"<?xml version=\"1.0\" ?>\xf0\xe9\x88\x14<response value=\"NAK\"/>";
        let resp = getresponse(input);
        assert_eq!(resp.iter().find(|(k, _)| k == "value").unwrap().1, "NAK");
    }
}
