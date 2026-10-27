//! Trace layout of the time view (`View::signals_changed`): row heights
//! and each trace's place, in logic mode by view index.

/// `View::HeightUnit`: also the smallest row.
pub const HEIGHT_UNIT: f64 = 20.0;
/// `View::SignalMargin`, `SignalMarginCompact` (without channel dividers).
pub const SIGNAL_MARGIN: f64 = 12.0;
pub const SIGNAL_MARGIN_COMPACT: f64 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceKind {
    Logic,
    Decoder,
    Dso,
    Analog,
}

/// A trace to place.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    pub kind: TraceKind,
    /// Rows the trace takes (a decoder: its shown rows; a signal: 1).
    pub rows: usize,
    pub enabled: bool,
    /// Logic mode order; -1 puts a decoder first.
    pub view_index: i32,
}

/// Where a trace goes: `v_offset` is its vertical centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    /// Index into the slots given to [`layout`].
    pub slot: usize,
    pub v_offset: f64,
    pub total_height: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub signal_height: f64,
    /// Row pitch (`_spanY`).
    pub span_y: f64,
    pub placed: Vec<Placed>,
}

/// The view's dimensions and settings for [`layout`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutParams {
    /// Height of the time viewport.
    pub viewport_height: f64,
    /// `get_time_pane_height()`: the pane minus the status bar's overlap.
    pub time_pane_height: f64,
    /// Logic channel dividers on (`logicChannelDivider`).
    pub dividers: bool,
    /// Line height of the trace font.
    pub font_height: f64,
    /// The device's `SR_CONF_MAX_HEIGHT_VALUE`, if it has one.
    pub max_height_value: Option<u8>,
    /// `_trace_height_factor` (vertical zoom).
    pub height_factor: f64,
    /// DSO channels split into bands.
    pub dso_split: bool,
    /// Logic mode (ordering by view index, row-fitting).
    pub logic_mode: bool,
    /// DSO mode: the measurement bar's overlap (`get_status_overlap()`).
    pub dso_mode_overlap: Option<f64>,
}

/// `View::signals_changed` for the time view.
/// `View::SignalSnapGridSize`: dragged traces snap to a 10 px grid.
pub const SIGNAL_SNAP_GRID: i64 = 10;

/// `Header::mouseMoveEvent`: the traces dragged from offsets `starts` by
/// `delta` pixels land on the grid. None when nothing moves. Ctrl makes no
/// difference: it adds a trace to the selection at the press, and the
/// selection then drags together (Qt moved nothing while Ctrl was held, F29).
pub fn header_drag<T: Copy>(starts: &[(T, f64)], delta: f64) -> Option<Vec<(T, f64)>> {
    let snap = |y: f64| {
        let y = y as i64;
        // C integer division
        ((y + SIGNAL_SNAP_GRID / 2) / SIGNAL_SNAP_GRID * SIGNAL_SNAP_GRID) as f64
    };
    let moved: Vec<(T, f64)> = starts.iter().map(|&(t, y)| (t, snap(y + delta))).collect();
    moved
        .iter()
        .zip(starts)
        .any(|(a, b)| a.1 != b.1)
        .then_some(moved)
}

pub fn layout(slots: &[Slot], p: &LayoutParams) -> Layout {
    let margin = if p.dividers {
        SIGNAL_MARGIN
    } else {
        SIGNAL_MARGIN_COMPACT
    };
    let mut total_rows = 0usize;
    let mut label_size = 0usize;
    for s in slots {
        let dso = s.kind == TraceKind::Dso;
        if s.enabled || (dso && !p.dso_split) {
            total_rows += s.rows;
        }
        if s.rows != 0 {
            label_size += 1;
        }
    }
    let total_rows = total_rows.max(1) as f64;
    let height = (p.viewport_height - 2.0 * margin * label_size as f64) / total_rows;

    let signal_height = if p.logic_mode {
        let min_row = HEIGHT_UNIT.max((p.font_height + 4.0).floor());
        let max_height = p
            .max_height_value
            .map_or(HEIGHT_UNIT, |v| (v as f64 + 1.0) * HEIGHT_UNIT);
        let mut h = if height < 2.0 * margin {
            min_row.max((p.viewport_height - 2.0 * margin * label_size as f64) / total_rows)
        } else {
            min_row.max(if height >= max_height {
                max_height
            } else {
                height
            })
        };
        h = min_row.max(h * p.height_factor);
        // a whole number of rows (two or more) tiles the pane
        let pane = p.time_pane_height - margin;
        let pitch = h + 2.0 * margin;
        if pane > 0.0 && pitch > 0.0 {
            let mut rows = (pane / pitch).round() as i64;
            while rows >= 2 {
                let fitted = (pane / rows as f64).floor() - 2.0 * margin;
                if fitted >= min_row {
                    h = fitted;
                    break;
                }
                rows -= 1;
            }
        }
        h
    } else if let Some(overlap) = p.dso_mode_overlap {
        HEIGHT_UNIT
            .max((p.viewport_height - overlap - 2.0 * margin * label_size as f64) / total_rows)
    } else if height <= 0.0 {
        1.0
    } else {
        height.trunc()
    };

    // logic mode: decoders without a view index first, then by view index
    let mut order: Vec<usize> = (0..slots.len()).collect();
    if p.logic_mode {
        let (first, mut rest): (Vec<usize>, Vec<usize>) = order
            .iter()
            .partition(|&&i| slots[i].kind == TraceKind::Decoder && slots[i].view_index == -1);
        rest.sort_by_key(|&i| slots[i].view_index);
        order = first.into_iter().chain(rest).collect();
    }

    let mut next = margin;
    let mut placed = Vec::with_capacity(slots.len());
    for i in order {
        let s = &slots[i];
        if s.rows == 0 {
            continue;
        }
        if !s.enabled && s.kind == TraceKind::Dso && p.dso_split {
            placed.push(Placed {
                slot: i,
                v_offset: next,
                total_height: 0,
            });
            continue;
        }
        let h = signal_height * s.rows as f64;
        placed.push(Placed {
            slot: i,
            v_offset: next + 0.5 * h + margin,
            total_height: h as i32,
        });
        next += h + 2.0 * margin;
    }
    Layout {
        signal_height,
        span_y: signal_height + 2.0 * margin,
        placed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logic(n: usize) -> Vec<Slot> {
        (0..n)
            .map(|i| Slot {
                kind: TraceKind::Logic,
                rows: 1,
                enabled: true,
                view_index: i as i32,
            })
            .collect()
    }

    fn params(h: f64) -> LayoutParams {
        LayoutParams {
            viewport_height: h,
            time_pane_height: h,
            dividers: true,
            font_height: 15.0,
            max_height_value: Some(1),
            height_factor: 1.0,
            dso_split: false,
            logic_mode: true,
            dso_mode_overlap: None,
        }
    }

    #[test]
    fn rows_tile_the_pane() {
        // 16 channels in 1000 px: the 40 px max row (MAX_HEIGHT_VALUE 1)
        // with 24 px of margins is a 64 px pitch, refitted to whole rows
        let l = layout(&logic(16), &params(1000.0));
        let rows = ((1000.0 - 12.0) / l.span_y).round();
        assert_eq!(((1000.0 - 12.0) / rows).floor(), l.span_y);
        assert!(l.signal_height >= 20.0);
        // stacked one pitch apart, the first centred in its slot
        assert_eq!(l.placed[0].v_offset, 12.0 + l.signal_height / 2.0 + 12.0);
        assert_eq!(l.placed[1].v_offset - l.placed[0].v_offset, l.span_y);
    }

    #[test]
    fn decoders_without_index_come_first_then_view_order() {
        let mut s = logic(3);
        s[0].view_index = 5;
        s.push(Slot {
            kind: TraceKind::Decoder,
            rows: 3,
            enabled: true,
            view_index: -1,
        });
        s.push(Slot {
            kind: TraceKind::Decoder,
            rows: 2,
            enabled: true,
            view_index: 2,
        });
        let l = layout(&s, &params(1000.0));
        let order: Vec<usize> = l.placed.iter().map(|p| p.slot).collect();
        assert_eq!(order, [3, 1, 2, 4, 0]);
        assert_eq!(l.placed[0].total_height, (l.signal_height * 3.0) as i32);
    }

    #[test]
    fn header_drag_snaps() {
        let starts = [(1, 33.0), (2, 70.0)];
        assert_eq!(
            header_drag(&starts, 0.0).unwrap(),
            vec![(1, 30.0), (2, 70.0)]
        );
        assert_eq!(
            header_drag(&starts, 14.0).unwrap(),
            vec![(1, 50.0), (2, 80.0)]
        );
        assert!(header_drag(&[(1, 30.0)], 2.0).is_none());
        // F29: there is no Ctrl case any more; Qt moved nothing while Ctrl
        // was held (the GUI test drags with Ctrl held)
    }
}
