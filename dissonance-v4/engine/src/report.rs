// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io::Write;

use serde::Serialize;

use crate::{archive::Archive, campaign::Campaign};

#[derive(Serialize)]
struct CellLine<'a> {
    cell: u32,
    key: &'a [u64],
    found: u64,
    chosen: u64,
    changes: u64,
    empty_streak: u64,
    group: u32,
    first_parent_cell: Option<u32>,
    front: Vec<FrontLine<'a>>,
    out_edges: u64,
}

#[derive(Serialize)]
struct FrontLine<'a> {
    state: u32,
    route: u64,
    found: u64,
    scalars: &'a [u64],
}

#[derive(Serialize)]
struct Summary<'a> {
    work: u64,
    jobs: u64,
    cells: usize,
    groups: usize,
    holders: usize,
    states: usize,
    shifts: &'a [u32],
    edges: usize,
    span_medians: Vec<u32>,
}

pub fn summary_line(c: &Campaign) -> String {
    serde_json::to_string(&Summary {
        work: c.work,
        jobs: c.jobs,
        cells: c.archive.cells.len(),
        groups: c.archive.groups.len(),
        holders: c.archive.holders(),
        states: c.archive.states.len(),
        shifts: &c.archive.shifts,
        edges: c.archive.edges.len(),
        span_medians: c.span_medians(),
    })
    .unwrap_or_default()
}

pub fn write_cells(archive: &Archive, out: &mut dyn Write) -> std::io::Result<()> {
    let mut out_edges = vec![0u64; archive.cells.len()];
    for ((a, _), n) in &archive.edges {
        if let Some(slot) = out_edges.get_mut(*a as usize) {
            *slot += n;
        }
    }
    for (i, cell) in archive.cells.iter().enumerate() {
        let line = CellLine {
            cell: i as u32,
            key: &cell.key,
            found: cell.found,
            chosen: cell.chosen,
            changes: cell.changes,
            empty_streak: cell.empty_streak,
            group: cell.group,
            first_parent_cell: cell.first_parent_cell,
            front: cell
                .front
                .iter()
                .map(|&s| {
                    let st = &archive.states[s as usize];
                    FrontLine {
                        state: s,
                        route: st.route,
                        found: st.found,
                        scalars: &st.scalars,
                    }
                })
                .collect(),
            out_edges: out_edges[i],
        };
        serde_json::to_writer(&mut *out, &line)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}
