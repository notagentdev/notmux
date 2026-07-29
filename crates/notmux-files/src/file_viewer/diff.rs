//! Line-level diff used by the editable diff editor.
//!
//! Compares a baseline (the file at `HEAD`) against the current, editable
//! buffer and produces a render plan: which buffer lines are additions
//! (rendered green) and, at each boundary, which baseline lines were deleted
//! (rendered as red read-only rows between the buffer lines). Recomputed as the
//! buffer is edited, so the coloring stays live. A common-prefix/suffix trim
//! keeps the O(n·m) core running only over the actually-changed window, so a
//! keystroke in a large file stays cheap.

/// Per-line diff of the current buffer vs the baseline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineDiff {
    /// One entry per current buffer line: `true` when the line is an addition
    /// (does not exist in the baseline) — rendered with a green background.
    pub added: Vec<bool>,
    /// One entry per boundary `0..=buffer_line_count`: the baseline lines
    /// deleted immediately before that buffer line, as `(old_1based_line, text)`.
    /// Index `buffer_line_count` holds deletions after the final line.
    pub deleted_before_lines: Vec<Vec<(usize, String)>>,
}

/// One row in the diff editor's visual layout: either an editable buffer line
/// or a read-only deleted (baseline) line shown in red.
#[derive(Clone, Debug)]
pub enum DiffRow {
    /// Editable buffer line (index into the buffer / highlighted lines).
    Buffer(usize),
    /// Deleted baseline line: its old 1-based line number and text.
    Deleted { old_line: usize, text: String },
}

impl LineDiff {
    /// True when there are no additions or deletions to render.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.added.iter().all(|a| !a) && self.deleted_before_lines.iter().all(|d| d.is_empty())
    }

    /// Build the interleaved render plan: deleted rows appear before the buffer
    /// line at their boundary; trailing deletions come after the last line.
    pub fn rows(&self) -> Vec<DiffRow> {
        let line_count = self.added.len();
        let mut rows = Vec::with_capacity(line_count);
        for i in 0..line_count {
            if let Some(dels) = self.deleted_before_lines.get(i) {
                for (old_line, text) in dels {
                    rows.push(DiffRow::Deleted {
                        old_line: *old_line,
                        text: text.clone(),
                    });
                }
            }
            rows.push(DiffRow::Buffer(i));
        }
        if let Some(dels) = self.deleted_before_lines.get(line_count) {
            for (old_line, text) in dels {
                rows.push(DiffRow::Deleted {
                    old_line: *old_line,
                    text: text.clone(),
                });
            }
        }
        rows
    }

    /// Index into `rows()` of the first changed row — the first deleted
    /// baseline line or added buffer line. None when the diff is empty.
    pub fn first_change_row(&self) -> Option<usize> {
        let line_count = self.added.len();
        let mut row = 0;
        for i in 0..line_count {
            if self
                .deleted_before_lines
                .get(i)
                .is_some_and(|d| !d.is_empty())
            {
                return Some(row);
            }
            if self.added[i] {
                return Some(row);
            }
            row += 1;
        }
        self.deleted_before_lines
            .get(line_count)
            .is_some_and(|d| !d.is_empty())
            .then_some(row)
    }
}

/// Above this window size (after prefix/suffix trimming) we skip the O(n·m)
/// LCS to keep editing responsive; that hunk then shows no decorations.
const MAX_WINDOW_LINES: usize = 4000;

/// Compute line-level additions/deletions of `current` relative to `baseline`.
pub fn compute_line_diff(baseline: &str, current: &str) -> LineDiff {
    let a = split_lines(baseline); // old
    let b = split_lines(current); // new (buffer)
    let n = a.len();
    let m = b.len();

    let mut added = vec![false; m];
    let mut deleted_before_lines: Vec<Vec<(usize, String)>> = vec![Vec::new(); m + 1];

    // Trim the common prefix and suffix so the LCS only runs over the changed
    // middle — a single-line edit collapses the window to ~1 line.
    let mut p = 0;
    while p < n && p < m && a[p] == b[p] {
        p += 1;
    }
    let mut s = 0;
    while s < n - p && s < m - p && a[n - 1 - s] == b[m - 1 - s] {
        s += 1;
    }

    diff_window(
        &a[p..n - s],
        &b[p..m - s],
        p,
        p,
        &mut added,
        &mut deleted_before_lines,
    );

    LineDiff {
        added,
        deleted_before_lines,
    }
}

/// Diff the changed window `a` (old) vs `b` (new). `a_off`/`b_off` are the
/// offsets of these slices within the whole file so results land at global
/// indices. Fills `added` (global new-line flags) and `deleted` (global
/// boundary → deleted old lines).
fn diff_window(
    a: &[&str],
    b: &[&str],
    a_off: usize,
    b_off: usize,
    added: &mut [bool],
    deleted: &mut [Vec<(usize, String)>],
) {
    let n = a.len();
    let m = b.len();
    if n == 0 {
        for j in 0..m {
            added[b_off + j] = true;
        }
        return;
    }
    if m == 0 {
        for (i, line) in a.iter().enumerate() {
            deleted[b_off].push((a_off + i + 1, (*line).to_string()));
        }
        return;
    }
    if n > MAX_WINDOW_LINES || m > MAX_WINDOW_LINES {
        return;
    }

    // LCS length table: dp[i][j] = LCS length of a[i..] and b[j..].
    let stride = m + 1;
    let mut dp = vec![0u32; (n + 1) * stride];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * stride + j] = if a[i] == b[j] {
                dp[(i + 1) * stride + (j + 1)] + 1
            } else {
                dp[(i + 1) * stride + j].max(dp[i * stride + (j + 1)])
            };
        }
    }

    let mut i = 0;
    let mut j = 0;
    while i < n && j < m {
        if a[i] == b[j] {
            i += 1;
            j += 1;
        } else if dp[(i + 1) * stride + j] >= dp[i * stride + (j + 1)] {
            deleted[b_off + j].push((a_off + i + 1, a[i].to_string()));
            i += 1;
        } else {
            added[b_off + j] = true;
            j += 1;
        }
    }
    while i < n {
        deleted[b_off + j].push((a_off + i + 1, a[i].to_string()));
        i += 1;
    }
    while j < m {
        added[b_off + j] = true;
        j += 1;
    }
}

/// Split into lines without the trailing-newline artifact, matching how the
/// editor buffer counts lines (`"a\n"` is one line `"a"`).
fn split_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = s.split('\n').collect();
    if s.ends_with('\n') {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deleted_texts(d: &LineDiff) -> Vec<(usize, String)> {
        d.deleted_before_lines.iter().flatten().cloned().collect()
    }

    #[test]
    fn no_changes_yields_empty() {
        let d = compute_line_diff("a\nb\nc\n", "a\nb\nc\n");
        assert!(d.is_empty());
        assert_eq!(d.added, vec![false, false, false]);
    }

    #[test]
    fn pure_addition_in_middle() {
        let d = compute_line_diff("a\nb\n", "a\nX\nb\n");
        assert_eq!(d.added, vec![false, true, false]);
        assert!(deleted_texts(&d).is_empty());
    }

    #[test]
    fn first_change_row_none_when_no_changes() {
        let d = compute_line_diff("a\nb\nc\n", "a\nb\nc\n");
        assert_eq!(d.first_change_row(), None);
    }

    #[test]
    fn first_change_row_points_at_added_line() {
        let d = compute_line_diff("a\nb\n", "a\nX\nb\n");
        // Rows: a(0), X(1, added), b(2)
        assert_eq!(d.first_change_row(), Some(1));
    }

    #[test]
    fn first_change_row_points_at_deleted_row() {
        let d = compute_line_diff("a\nb\nc\n", "a\nc\n");
        // Rows: a(0), deleted "b"(1), c(2)
        assert_eq!(d.first_change_row(), Some(1));
    }

    #[test]
    fn first_change_row_trailing_deletion() {
        let d = compute_line_diff("a\nb\n", "a\n");
        // Rows: a(0), trailing deleted "b"(1)
        assert_eq!(d.first_change_row(), Some(1));
    }

    #[test]
    fn pure_deletion_in_middle_keeps_text_and_number() {
        let d = compute_line_diff("a\nb\nc\n", "a\nc\n");
        assert_eq!(d.added, vec![false, false]);
        // "b" was old line 2, deleted before current line 1 ("c").
        assert_eq!(d.deleted_before_lines[1], vec![(2, "b".to_string())]);
    }

    #[test]
    fn replacement_is_add_plus_delete() {
        let d = compute_line_diff("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(d.added, vec![false, true, false]);
        assert_eq!(deleted_texts(&d), vec![(2, "b".to_string())]);
    }

    #[test]
    fn everything_new_and_everything_deleted() {
        let all_new = compute_line_diff("", "x\ny\n");
        assert_eq!(all_new.added, vec![true, true]);

        let all_gone = compute_line_diff("x\ny\n", "");
        assert_eq!(
            all_gone.deleted_before_lines[0],
            vec![(1, "x".to_string()), (2, "y".to_string())]
        );
    }

    #[test]
    fn trailing_deletion_after_last_line() {
        let d = compute_line_diff("a\nb\nc\n", "a\n");
        assert_eq!(d.added, vec![false]);
        assert_eq!(
            d.deleted_before_lines[1],
            vec![(2, "b".to_string()), (3, "c".to_string())]
        );
    }

    #[test]
    fn rows_interleave_deleted_before_buffer() {
        // old: a,b,c ; new: a,c  → rows: Buffer(0), Deleted(b), Buffer(1)
        let d = compute_line_diff("a\nb\nc\n", "a\nc\n");
        let rows = d.rows();
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0], DiffRow::Buffer(0)));
        assert!(matches!(&rows[1], DiffRow::Deleted { old_line: 2, text } if text == "b"));
        assert!(matches!(rows[2], DiffRow::Buffer(1)));
    }

    #[test]
    fn prefix_suffix_trim_matches_naive_addition() {
        // A change deep in a large file must still be located correctly.
        let mut old = String::new();
        let mut new = String::new();
        for i in 0..500 {
            old.push_str(&format!("line{i}\n"));
            new.push_str(&format!("line{i}\n"));
            if i == 250 {
                new.push_str("INSERTED\n");
            }
        }
        let d = compute_line_diff(&old, &new);
        // Exactly one addition, at the inserted position (buffer line 251).
        assert_eq!(d.added.iter().filter(|x| **x).count(), 1);
        assert!(d.added[251]);
        assert!(deleted_texts(&d).is_empty());
    }
}
