#![forbid(unsafe_code)]

use std::collections::BTreeMap;

const MAX_DIAGNOSTICS: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryWindow {
    pub base_absolute_line: u64,
    pub total_lines: usize,
    pub scrollback_lines: usize,
    pub cols: usize,
    pub rows: usize,
}

impl HistoryWindow {
    #[must_use]
    pub fn max_relative_line(self) -> Option<usize> {
        self.total_lines.checked_sub(1)
    }

    #[must_use]
    pub fn absolute_line_for_relative(self, relative_line: usize) -> Option<u64> {
        if relative_line >= self.total_lines {
            return None;
        }
        let rel = u64::try_from(relative_line).ok()?;
        Some(self.base_absolute_line.saturating_add(rel))
    }

    #[must_use]
    pub fn relative_line_for_absolute(self, absolute_line: u64) -> Option<usize> {
        let delta = absolute_line.checked_sub(self.base_absolute_line)?;
        let rel = usize::try_from(delta).ok()?;
        (rel < self.total_lines).then_some(rel)
    }

    #[must_use]
    pub fn grid_row_for_absolute(self, absolute_line: u64) -> Option<usize> {
        let rel = self.relative_line_for_absolute(absolute_line)?;
        if rel < self.scrollback_lines {
            return None;
        }
        let row = rel.saturating_sub(self.scrollback_lines);
        (row < self.rows).then_some(row)
    }

    #[must_use]
    pub fn cell_offset(self, absolute_line: u64, column: u16) -> Option<u32> {
        if self.cols == 0 {
            return None;
        }
        let row = self.grid_row_for_absolute(absolute_line)?;
        let col_max = self.cols.saturating_sub(1);
        let col = usize::from(column).min(col_max);
        let offset = row.saturating_mul(self.cols).saturating_add(col);
        u32::try_from(offset).ok()
    }

    #[must_use]
    pub fn grid_absolute_range(self) -> Option<(u64, u64)> {
        if self.rows == 0 {
            return None;
        }
        let start_rel = self.scrollback_lines;
        if start_rel >= self.total_lines {
            return None;
        }
        let end_rel_exclusive = start_rel.saturating_add(self.rows).min(self.total_lines);
        if end_rel_exclusive <= start_rel {
            return None;
        }
        let start_rel_u64 = u64::try_from(start_rel).ok()?;
        let end_rel_u64 = u64::try_from(end_rel_exclusive).ok()?;
        let start = self.base_absolute_line.saturating_add(start_rel_u64);
        let end_exclusive = self.base_absolute_line.saturating_add(end_rel_u64);
        Some((start, end_exclusive))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationKind {
    Inline,
    Line,
    Range,
}

impl DecorationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Line => "line",
            Self::Range => "range",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "inline" => Some(Self::Inline),
            "line" => Some(Self::Line),
            "range" => Some(Self::Range),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticEntity {
    Marker,
    Decoration,
}

impl DiagnosticEntity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Marker => "marker",
            Self::Decoration => "decoration",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticEvent {
    pub seq: u64,
    pub entity: DiagnosticEntity,
    pub action: &'static str,
    pub id: u32,
    pub reason: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkerRecord {
    id: u32,
    absolute_line: u64,
    column: u16,
    stale_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecorationRecord {
    id: u32,
    kind: DecorationKind,
    start_marker_id: u32,
    end_marker_id: Option<u32>,
    start_col: u16,
    end_col: u16,
    stale_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkerSnapshot {
    pub id: u32,
    pub absolute_line: u64,
    pub column: u16,
    pub stale: bool,
    pub stale_reason: Option<&'static str>,
    pub relative_line: Option<usize>,
    pub grid_row: Option<usize>,
    pub cell_offset: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecorationSnapshot {
    pub id: u32,
    pub kind: DecorationKind,
    pub start_marker_id: u32,
    pub end_marker_id: Option<u32>,
    pub stale: bool,
    pub stale_reason: Option<&'static str>,
    pub start_offset: Option<u32>,
    pub end_offset: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct MarkerStore {
    next_marker_id: u32,
    next_decoration_id: u32,
    diag_seq: u64,
    markers: BTreeMap<u32, MarkerRecord>,
    decorations: BTreeMap<u32, DecorationRecord>,
    diagnostics: Vec<DiagnosticEvent>,
}

impl MarkerStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_marker_id: 1,
            next_decoration_id: 1,
            diag_seq: 1,
            markers: BTreeMap::new(),
            decorations: BTreeMap::new(),
            diagnostics: Vec::new(),
        }
    }

    fn next_free_marker_id(&mut self) -> u32 {
        loop {
            let id = self.next_marker_id;
            self.next_marker_id = self.next_marker_id.saturating_add(1).max(1);
            if !self.markers.contains_key(&id) {
                return id;
            }
        }
    }

    fn next_free_decoration_id(&mut self) -> u32 {
        loop {
            let id = self.next_decoration_id;
            self.next_decoration_id = self.next_decoration_id.saturating_add(1).max(1);
            if !self.decorations.contains_key(&id) {
                return id;
            }
        }
    }

    fn push_diag(
        &mut self,
        entity: DiagnosticEntity,
        action: &'static str,
        id: u32,
        reason: Option<&'static str>,
    ) {
        if self.diagnostics.len() >= MAX_DIAGNOSTICS {
            let overflow = self.diagnostics.len() - MAX_DIAGNOSTICS + 1;
            self.diagnostics.drain(..overflow);
        }
        self.diagnostics.push(DiagnosticEvent {
            seq: self.diag_seq,
            entity,
            action,
            id,
            reason,
        });
        self.diag_seq = self.diag_seq.saturating_add(1);
    }

    pub fn create_marker(
        &mut self,
        relative_line: usize,
        column: u16,
        window: HistoryWindow,
    ) -> Result<u32, &'static str> {
        let absolute_line = window
            .absolute_line_for_relative(relative_line)
            .ok_or("line index out of range")?;
        let clamped_col = if window.cols == 0 {
            0
        } else {
            let max_col = window.cols.saturating_sub(1);
            let col = usize::from(column).min(max_col);
            u16::try_from(col).unwrap_or(u16::MAX)
        };

        let id = self.next_free_marker_id();
        self.markers.insert(
            id,
            MarkerRecord {
                id,
                absolute_line,
                column: clamped_col,
                stale_reason: None,
            },
        );
        self.push_diag(DiagnosticEntity::Marker, "created", id, None);
        self.reconcile(window);
        Ok(id)
    }

    pub fn remove_marker(&mut self, marker_id: u32, window: HistoryWindow) -> bool {
        let removed = self.markers.remove(&marker_id).is_some();
        if removed {
            self.push_diag(DiagnosticEntity::Marker, "removed", marker_id, None);
            self.reconcile(window);
        }
        removed
    }

    pub fn create_decoration(
        &mut self,
        kind: DecorationKind,
        start_marker_id: u32,
        end_marker_id: Option<u32>,
        start_col: u16,
        end_col: u16,
        window: HistoryWindow,
    ) -> Result<u32, &'static str> {
        if !self.markers.contains_key(&start_marker_id) {
            return Err("start marker not found");
        }
        let has_end_marker = end_marker_id
            .map(|id| self.markers.contains_key(&id))
            .unwrap_or(false);
        if matches!(kind, DecorationKind::Range) && !has_end_marker {
            return Err("range decorations require an existing end marker");
        }

        let (normalized_start_col, normalized_end_col) = if window.cols == 0 {
            (0, 0)
        } else {
            let max_col = u16::try_from(window.cols).unwrap_or(u16::MAX);
            let start = start_col.min(max_col);
            let mut end = end_col.min(max_col);
            if matches!(kind, DecorationKind::Inline | DecorationKind::Range) && end <= start {
                end = start.saturating_add(1).min(max_col);
            }
            (start, end)
        };

        let id = self.next_free_decoration_id();
        self.decorations.insert(
            id,
            DecorationRecord {
                id,
                kind,
                start_marker_id,
                end_marker_id,
                start_col: normalized_start_col,
                end_col: normalized_end_col,
                stale_reason: None,
            },
        );
        self.push_diag(DiagnosticEntity::Decoration, "created", id, None);
        self.reconcile(window);
        Ok(id)
    }

    pub fn remove_decoration(&mut self, decoration_id: u32) -> bool {
        let removed = self.decorations.remove(&decoration_id).is_some();
        if removed {
            self.push_diag(DiagnosticEntity::Decoration, "removed", decoration_id, None);
        }
        removed
    }

    pub fn reconcile(&mut self, window: HistoryWindow) {
        let mut marker_changes = Vec::new();
        for marker in self.markers.values_mut() {
            let next_reason = if window.total_lines == 0 {
                Some("history_empty")
            } else {
                let oldest = window.base_absolute_line;
                let newest_exclusive =
                    oldest.saturating_add(u64::try_from(window.total_lines).unwrap_or(u64::MAX));
                if marker.absolute_line < oldest {
                    Some("compacted_out")
                } else if marker.absolute_line >= newest_exclusive {
                    Some("outside_window")
                } else {
                    None
                }
            };
            if marker.stale_reason != next_reason {
                marker.stale_reason = next_reason;
                marker_changes.push((marker.id, next_reason));
            }
        }

        for (id, reason) in marker_changes {
            let action = if reason.is_some() {
                "invalidated"
            } else {
                "revalidated"
            };
            self.push_diag(DiagnosticEntity::Marker, action, id, reason);
        }

        let mut decoration_changes = Vec::new();
        let decoration_ids: Vec<u32> = self.decorations.keys().copied().collect();
        for decoration_id in decoration_ids {
            let Some(current) = self.decorations.get(&decoration_id).copied() else {
                continue;
            };
            let next_reason = self.decoration_stale_reason(current);
            if current.stale_reason != next_reason
                && let Some(decoration) = self.decorations.get_mut(&decoration_id)
            {
                decoration.stale_reason = next_reason;
                decoration_changes.push((decoration.id, next_reason));
            }
        }

        for (id, reason) in decoration_changes {
            let action = if reason.is_some() {
                "invalidated"
            } else {
                "revalidated"
            };
            self.push_diag(DiagnosticEntity::Decoration, action, id, reason);
        }
    }

    fn decoration_stale_reason(&self, decoration: DecorationRecord) -> Option<&'static str> {
        let Some(start_marker) = self.markers.get(&decoration.start_marker_id) else {
            return Some("start_marker_missing");
        };
        if start_marker.stale_reason.is_some() {
            return Some("start_marker_stale");
        }

        if matches!(decoration.kind, DecorationKind::Range) {
            let Some(end_id) = decoration.end_marker_id else {
                return Some("end_marker_missing");
            };
            let Some(end_marker) = self.markers.get(&end_id) else {
                return Some("end_marker_missing");
            };
            if end_marker.stale_reason.is_some() {
                return Some("end_marker_stale");
            }
        }

        None
    }

    #[must_use]
    pub fn marker_snapshots(&self, window: HistoryWindow) -> Vec<MarkerSnapshot> {
        self.markers
            .values()
            .map(|marker| {
                let relative_line = window.relative_line_for_absolute(marker.absolute_line);
                let grid_row = window.grid_row_for_absolute(marker.absolute_line);
                let cell_offset = window.cell_offset(marker.absolute_line, marker.column);
                MarkerSnapshot {
                    id: marker.id,
                    absolute_line: marker.absolute_line,
                    column: marker.column,
                    stale: marker.stale_reason.is_some(),
                    stale_reason: marker.stale_reason,
                    relative_line,
                    grid_row,
                    cell_offset,
                }
            })
            .collect()
    }

    #[must_use]
    pub fn decoration_snapshots(&self, window: HistoryWindow) -> Vec<DecorationSnapshot> {
        self.decorations
            .values()
            .map(|decoration| {
                let stale_reason = decoration.stale_reason;
                let (start_offset, end_offset) = if stale_reason.is_some() {
                    (None, None)
                } else {
                    self.resolve_decoration_offsets(*decoration, window)
                };
                DecorationSnapshot {
                    id: decoration.id,
                    kind: decoration.kind,
                    start_marker_id: decoration.start_marker_id,
                    end_marker_id: decoration.end_marker_id,
                    stale: stale_reason.is_some(),
                    stale_reason,
                    start_offset,
                    end_offset,
                }
            })
            .collect()
    }

    fn resolve_decoration_offsets(
        &self,
        decoration: DecorationRecord,
        window: HistoryWindow,
    ) -> (Option<u32>, Option<u32>) {
        match decoration.kind {
            DecorationKind::Inline => {
                let Some(marker) = self.markers.get(&decoration.start_marker_id) else {
                    return (None, None);
                };
                let Some(line_start) = window.cell_offset(marker.absolute_line, 0) else {
                    return (None, None);
                };
                let cols_u32 = u32::try_from(window.cols).unwrap_or(u32::MAX);
                if cols_u32 == 0 {
                    return (None, None);
                }
                let start = line_start.saturating_add(u32::from(decoration.start_col));
                let mut end = line_start.saturating_add(u32::from(decoration.end_col));
                let line_end = line_start.saturating_add(cols_u32);
                end = end.min(line_end);
                if end <= start {
                    end = start.saturating_add(1).min(line_end);
                }
                (Some(start), Some(end))
            }
            DecorationKind::Line => {
                let Some(marker) = self.markers.get(&decoration.start_marker_id) else {
                    return (None, None);
                };
                let Some(line_start) = window.cell_offset(marker.absolute_line, 0) else {
                    return (None, None);
                };
                let cols_u32 = u32::try_from(window.cols).unwrap_or(u32::MAX);
                if cols_u32 == 0 {
                    return (None, None);
                }
                (Some(line_start), Some(line_start.saturating_add(cols_u32)))
            }
            DecorationKind::Range => {
                let Some(start_marker) = self.markers.get(&decoration.start_marker_id) else {
                    return (None, None);
                };
                let Some(end_marker_id) = decoration.end_marker_id else {
                    return (None, None);
                };
                let Some(end_marker) = self.markers.get(&end_marker_id) else {
                    return (None, None);
                };

                let (mut start_abs, mut end_abs) =
                    (start_marker.absolute_line, end_marker.absolute_line);
                let (mut start_col, mut end_col) = (decoration.start_col, decoration.end_col);
                if end_abs < start_abs {
                    std::mem::swap(&mut start_abs, &mut end_abs);
                    std::mem::swap(&mut start_col, &mut end_col);
                }

                let Some((grid_abs_start, grid_abs_end_exclusive)) = window.grid_absolute_range()
                else {
                    return (None, None);
                };
                let clipped_start_abs = start_abs.max(grid_abs_start);
                let clipped_end_abs = end_abs.min(grid_abs_end_exclusive.saturating_sub(1));
                if clipped_end_abs < clipped_start_abs {
                    return (None, None);
                }

                let clipped_start_col = if clipped_start_abs == start_abs {
                    start_col
                } else {
                    0
                };
                let clipped_end_col = if clipped_end_abs == end_abs {
                    end_col
                } else {
                    u16::try_from(window.cols).unwrap_or(u16::MAX)
                };

                let start_offset = window.cell_offset(clipped_start_abs, clipped_start_col);
                let end_offset = window.cell_offset(clipped_end_abs, clipped_end_col);
                match (start_offset, end_offset) {
                    (Some(start), Some(mut end)) => {
                        if end <= start {
                            end = start.saturating_add(1);
                        }
                        (Some(start), Some(end))
                    }
                    _ => (None, None),
                }
            }
        }
    }

    #[must_use]
    pub fn marker_count(&self) -> usize {
        self.markers.len()
    }

    #[must_use]
    pub fn decoration_count(&self) -> usize {
        self.decorations.len()
    }

    pub fn drain_diagnostics(&mut self) -> Vec<DiagnosticEvent> {
        self.diagnostics.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(
        base: u64,
        total: usize,
        scrollback: usize,
        cols: usize,
        rows: usize,
    ) -> HistoryWindow {
        HistoryWindow {
            base_absolute_line: base,
            total_lines: total,
            scrollback_lines: scrollback,
            cols,
            rows,
        }
    }

    #[test]
    fn marker_anchor_survives_base_shift_until_compacted_out() {
        let mut store = MarkerStore::new();
        let w0 = window(0, 20, 10, 80, 10);
        let marker_id = store
            .create_marker(12, 7, w0)
            .expect("marker should be created");

        let s0 = store
            .marker_snapshots(w0)
            .into_iter()
            .find(|m| m.id == marker_id)
            .expect("marker snapshot should exist");
        assert!(!s0.stale);
        assert_eq!(s0.relative_line, Some(12));
        assert_eq!(s0.grid_row, Some(2));

        let w1 = window(5, 20, 10, 80, 10);
        store.reconcile(w1);
        let s1 = store
            .marker_snapshots(w1)
            .into_iter()
            .find(|m| m.id == marker_id)
            .expect("marker snapshot should exist");
        assert!(!s1.stale);
        assert_eq!(s1.relative_line, Some(7));
        assert_eq!(s1.grid_row, None);

        let w2 = window(13, 20, 10, 80, 10);
        store.reconcile(w2);
        let s2 = store
            .marker_snapshots(w2)
            .into_iter()
            .find(|m| m.id == marker_id)
            .expect("marker snapshot should exist");
        assert!(s2.stale);
        assert_eq!(s2.stale_reason, Some("compacted_out"));
    }

    #[test]
    fn inline_decoration_tracks_marker_visibility_and_invalidation() {
        let mut store = MarkerStore::new();
        let w0 = window(0, 8, 4, 16, 4);
        let marker_id = store
            .create_marker(6, 2, w0)
            .expect("marker should be created");
        let decoration_id = store
            .create_decoration(DecorationKind::Inline, marker_id, None, 1, 5, w0)
            .expect("decoration should be created");

        let d0 = store
            .decoration_snapshots(w0)
            .into_iter()
            .find(|d| d.id == decoration_id)
            .expect("decoration snapshot should exist");
        assert!(!d0.stale);
        assert!(d0.start_offset.is_some());
        assert!(d0.end_offset.is_some());

        let w1 = window(7, 8, 4, 16, 4);
        store.reconcile(w1);
        let d1 = store
            .decoration_snapshots(w1)
            .into_iter()
            .find(|d| d.id == decoration_id)
            .expect("decoration snapshot should exist");
        assert!(d1.stale);
        assert_eq!(d1.stale_reason, Some("start_marker_stale"));
    }

    #[test]
    fn range_decoration_clamps_to_visible_grid_window() {
        let mut store = MarkerStore::new();
        let w = window(100, 20, 10, 10, 10);
        let start = store
            .create_marker(8, 2, w)
            .expect("start marker should be created");
        let end = store
            .create_marker(18, 8, w)
            .expect("end marker should be created");
        let decoration_id = store
            .create_decoration(DecorationKind::Range, start, Some(end), 2, 8, w)
            .expect("range decoration should be created");

        let snapshot = store
            .decoration_snapshots(w)
            .into_iter()
            .find(|d| d.id == decoration_id)
            .expect("decoration snapshot should exist");
        assert!(!snapshot.stale);
        assert_eq!(snapshot.start_offset, Some(0));
        assert_eq!(snapshot.end_offset, Some(88));
    }

    #[test]
    fn removing_marker_invalidates_dependent_range_decoration() {
        let mut store = MarkerStore::new();
        let w = window(0, 12, 6, 12, 6);
        let start = store
            .create_marker(7, 1, w)
            .expect("start marker should be created");
        let end = store
            .create_marker(9, 1, w)
            .expect("end marker should be created");
        let decoration_id = store
            .create_decoration(DecorationKind::Range, start, Some(end), 1, 2, w)
            .expect("range decoration should be created");

        assert!(store.remove_marker(start, w));
        let snapshot = store
            .decoration_snapshots(w)
            .into_iter()
            .find(|d| d.id == decoration_id)
            .expect("decoration snapshot should exist");
        assert!(snapshot.stale);
        assert_eq!(snapshot.stale_reason, Some("start_marker_missing"));
    }

    #[test]
    fn diagnostics_are_bounded_and_monotonic() {
        let mut store = MarkerStore::new();
        let w = window(0, 4, 2, 4, 2);
        let id = store
            .create_marker(0, 0, w)
            .expect("marker should be created");
        assert!(store.remove_marker(id, w));

        let diags = store.drain_diagnostics();
        assert!(!diags.is_empty());
        for pair in diags.windows(2) {
            assert!(pair[0].seq < pair[1].seq);
        }
    }
}
