use crate::git::Commit;
use git2::Oid;

pub const PALETTE_SIZE: usize = 8;

/// A vertical slice of the graph for one commit row.
///
/// Lanes are columns. Lines are drawn in two halves around the commit node:
/// `incoming` runs from the top edge of the row to the node, `outgoing` from the node
/// to the bottom edge, and `through` lanes simply cross the row untouched.
#[derive(Clone, Debug, Default)]
pub struct GraphRow {
    pub lane: usize,
    pub color: usize,
    pub through: Vec<(usize, usize)>,
    pub incoming: Vec<(usize, usize)>,
    pub outgoing: Vec<(usize, usize)>,
}

pub struct Graph {
    pub rows: Vec<GraphRow>,
    pub width: usize,
}

pub fn layout(commits: &[Commit]) -> Graph {
    // Each slot waits for a commit id and remembers the color of the line leading there.
    let mut lanes: Vec<Option<(Oid, usize)>> = Vec::new();
    let mut next_color = 0usize;
    let mut width = 1usize;
    let mut rows = Vec::with_capacity(commits.len());
    let mut new_color = || {
        let c = next_color % PALETTE_SIZE;
        next_color += 1;
        c
    };

    for commit in commits {
        let waiting: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(_, l)| matches!(l, Some((oid, _)) if *oid == commit.oid))
            .map(|(i, _)| i)
            .collect();

        let lane = match waiting.first() {
            Some(&i) => i,
            None => free_slot(&mut lanes),
        };
        let color = match waiting.first() {
            Some(&i) => lanes[i].map_or(0, |(_, c)| c),
            None => new_color(),
        };

        let mut row = GraphRow { lane, color, ..GraphRow::default() };
        for (i, slot) in lanes.iter().enumerate() {
            let Some((oid, c)) = *slot else { continue };
            if oid == commit.oid {
                row.incoming.push((i, c));
            } else {
                row.through.push((i, c));
            }
        }
        for &i in &waiting {
            lanes[i] = None;
        }

        for (n, parent) in commit.parents.iter().enumerate() {
            let existing = lanes
                .iter()
                .position(|l| matches!(l, Some((oid, _)) if oid == parent));
            match existing {
                Some(i) => {
                    let c = lanes[i].map_or(color, |(_, c)| c);
                    row.outgoing.push((i, c));
                }
                None => {
                    let (target, c) = if n == 0 {
                        if lane >= lanes.len() {
                            lanes.resize(lane + 1, None);
                        }
                        (lane, color)
                    } else {
                        (free_slot(&mut lanes), new_color())
                    };
                    lanes[target] = Some((*parent, c));
                    row.outgoing.push((target, c));
                }
            }
        }

        while matches!(lanes.last(), Some(None)) {
            lanes.pop();
        }
        width = width.max(lanes.len()).max(lane + 1);
        rows.push(row);
    }

    Graph { rows, width }
}

fn free_slot(lanes: &mut Vec<Option<(Oid, usize)>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}
