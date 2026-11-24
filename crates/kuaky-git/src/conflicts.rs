//! Parsing and resolving standard Git merge conflict markers (`<<<<<<<`, `=======`, `>>>>>>>`).

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Ours,
    Theirs,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictBlock {
    pub id: usize,
    pub start_line: usize,
    pub ours_label: String,
    pub ours: String,
    pub theirs_label: String,
    pub theirs: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Conflict(ConflictBlock),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictFile {
    pub segments: Vec<Segment>,
}

impl ConflictFile {
    pub fn parse(content: &str) -> Self {
        let mut segments = Vec::new();
        let mut plain = String::new();
        let mut next_id = 0usize;

        let mut lines = content.split_inclusive('\n').enumerate().peekable();
        while let Some((line_idx, line)) = lines.next() {
            if let Some(rest) = line.strip_prefix("<<<<<<<") {
                if !plain.is_empty() {
                    segments.push(Segment::Text(std::mem::take(&mut plain)));
                }
                let ours_label = rest.trim().to_string();
                let mut ours = String::new();
                let mut theirs = String::new();
                let mut theirs_label = String::new();
                let mut in_base = false;
                let mut in_theirs = false;
                let mut closed = false;

                for (_, inner) in lines.by_ref() {
                    if !in_theirs && inner.starts_with("|||||||") {
                        in_base = true;
                        continue;
                    }
                    if !in_theirs && inner.starts_with("=======") {
                        in_base = false;
                        in_theirs = true;
                        continue;
                    }
                    if in_theirs {
                        if let Some(end_rest) = inner.strip_prefix(">>>>>>>") {
                            theirs_label = end_rest.trim().to_string();
                            closed = true;
                            break;
                        }
                        theirs.push_str(inner);
                    } else if !in_base {
                        ours.push_str(inner);
                    }
                }

                if closed {
                    segments.push(Segment::Conflict(ConflictBlock {
                        id: next_id,
                        start_line: line_idx + 1,
                        ours_label,
                        ours,
                        theirs_label,
                        theirs,
                    }));
                    next_id += 1;
                } else {
                    // Malformed / unclosed marker: preserve text.
                    plain.push_str(line);
                    plain.push_str(&ours);
                    if in_theirs {
                        plain.push_str("=======\n");
                        plain.push_str(&theirs);
                    }
                }
            } else {
                plain.push_str(line);
            }
        }

        if !plain.is_empty() {
            segments.push(Segment::Text(plain));
        }

        Self { segments }
    }

    pub fn conflict_count(&self) -> usize {
        self.segments.iter().filter(|s| matches!(s, Segment::Conflict(_))).count()
    }

    pub fn blocks(&self) -> Vec<&ConflictBlock> {
        self.segments
            .iter()
            .filter_map(|s| match s {
                Segment::Conflict(b) => Some(b),
                Segment::Text(_) => None,
            })
            .collect()
    }

    /// Reconstructs the file replacing any conflict blocks present in `choices`.
    pub fn render_with(&self, choices: &HashMap<usize, ConflictChoice>) -> String {
        let mut out = String::new();
        for seg in &self.segments {
            match seg {
                Segment::Text(t) => out.push_str(t),
                Segment::Conflict(b) => match choices.get(&b.id) {
                    Some(ConflictChoice::Ours) => out.push_str(&b.ours),
                    Some(ConflictChoice::Theirs) => out.push_str(&b.theirs),
                    Some(ConflictChoice::Both) => {
                        out.push_str(&b.ours);
                        out.push_str(&b.theirs);
                    }
                    None => {
                        out.push_str("<<<<<<<");
                        if !b.ours_label.is_empty() {
                            out.push(' ');
                            out.push_str(&b.ours_label);
                        }
                        out.push('\n');
                        out.push_str(&b.ours);
                        out.push_str("=======\n");
                        out.push_str(&b.theirs);
                        out.push_str(">>>>>>>");
                        if !b.theirs_label.is_empty() {
                            out.push(' ');
                            out.push_str(&b.theirs_label);
                        }
                        out.push('\n');
                    }
                },
            }
        }
        out
    }
}

pub fn has_conflict_markers(content: &str) -> bool {
    ConflictFile::parse(content).conflict_count() > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_resolves_standard_and_diff3_conflicts() {
        let sample = "before\n<<<<<<< HEAD\nours line\n||||||| base\nold base\n=======\ntheirs line\n>>>>>>> feature\nafter\n";
        let file = ConflictFile::parse(sample);
        assert_eq!(file.conflict_count(), 1);
        let b = file.blocks()[0];
        assert_eq!(b.ours_label, "HEAD");
        assert_eq!(b.theirs_label, "feature");
        assert_eq!(b.ours, "ours line\n");
        assert_eq!(b.theirs, "theirs line\n");

        let mut c = HashMap::new();
        c.insert(0, ConflictChoice::Ours);
        assert_eq!(file.render_with(&c), "before\nours line\nafter\n");

        c.insert(0, ConflictChoice::Theirs);
        assert_eq!(file.render_with(&c), "before\ntheirs line\nafter\n");

        c.insert(0, ConflictChoice::Both);
        assert_eq!(file.render_with(&c), "before\nours line\ntheirs line\nafter\n");
        assert!(!has_conflict_markers(&file.render_with(&c)));
    }
}
