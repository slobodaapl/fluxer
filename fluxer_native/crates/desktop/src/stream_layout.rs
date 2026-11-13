// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local viewer layout only. It never changes subscriptions, focus or pixels.
//! Fluxer's VoiceCallLayout defaults to grid; its grid metrics choose layouts
//! by contained video area. Here we also consider each video's actual aspect.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StreamLayout {
    #[default]
    Tiled,
    MainStage,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

const DEFAULT_ASPECT: f32 = 16. / 9.;
// The source compact grid minimum is 148 × 83.25. Applying its shorter edge
// to portrait videos keeps both orientations legible without imposing 16:9.
const MIN_SHORT_EDGE: f64 = 148. * 9. / 16.;
const EXHAUSTIVE_COUNT: usize = 10;

fn aspect(value: f32) -> f64 {
    if value.is_finite() && value > 0. {
        f64::from(value.clamp(1. / 16., 16.))
    } else {
        f64::from(DEFAULT_ASPECT)
    }
}

struct Candidate {
    tiles: Vec<TileRect>,
    area: f64,
    shortest: f64,
}

impl Candidate {
    fn better_than(&self, other: &Self) -> bool {
        let legible = self.shortest + 0.001 >= MIN_SHORT_EDGE;
        let other_legible = other.shortest + 0.001 >= MIN_SHORT_EDGE;
        if legible != other_legible {
            return legible;
        }
        // If no layout can meet the compact minimum, avoid sacrificing one
        // stream to a postage stamp merely to enlarge its neighbors.
        if !legible && (self.shortest - other.shortest).abs() > 0.01 {
            return self.shortest > other.shortest;
        }
        if (self.area - other.area).abs() > 0.01 {
            return self.area > other.area;
        }
        self.shortest > other.shortest + 0.01
    }
}

/// Rectangles preserve stream order and fit the actual stage. Multiple-video
/// rectangles have their sanitized video aspect, with remaining stage space
/// centered as letterboxing. A single video gets the entire stage, allowing
/// the renderer's Contain fit to use all available space. Empty/invalid stages
/// return no tiles; invalid aspects use 16:9, extreme aspects are bounded.
pub fn tiled_layout(width: f32, height: f32, aspect_ratios: &[f32], gap: f32) -> Vec<TileRect> {
    if !width.is_finite()
        || !height.is_finite()
        || width <= 0.
        || height <= 0.
        || aspect_ratios.is_empty()
    {
        return Vec::new();
    }
    if aspect_ratios.len() == 1 {
        return vec![TileRect {
            x: 0.,
            y: 0.,
            width,
            height,
        }];
    }
    let aspects: Vec<_> = aspect_ratios.iter().map(|value| aspect(*value)).collect();
    let width = f64::from(width);
    let height = f64::from(height);
    let gap = if gap.is_finite() {
        f64::from(gap.max(0.))
    } else {
        0.
    };
    // Even pathological tiny stages/counts retain space for every stream.
    let gap = gap.min(width.min(height) / (2. * aspects.len() as f64));
    let mut best: Option<Candidate> = None;
    for transpose in [false, true] {
        let (stage_width, stage_height) = if transpose {
            (height, width)
        } else {
            (width, height)
        };
        let ratios: Vec<_> = aspects
            .iter()
            .map(|ratio| if transpose { 1. / ratio } else { *ratio })
            .collect();
        for partition in partitions(&ratios) {
            let uniform = arrange(stage_width, stage_height, &ratios, &partition, gap);
            let mut candidates = nonuniform_arrangements(
                stage_width,
                stage_height,
                &ratios,
                &partition,
                gap,
                uniform.shortest,
            );
            candidates.push(uniform);
            for mut candidate in candidates {
                if transpose {
                    for tile in &mut candidate.tiles {
                        std::mem::swap(&mut tile.x, &mut tile.y);
                        std::mem::swap(&mut tile.width, &mut tile.height);
                    }
                }
                if best.as_ref().is_none_or(|best| candidate.better_than(best)) {
                    best = Some(candidate);
                }
            }
        }
    }
    best.map_or_else(Vec::new, |best| best.tiles)
}

/// For normal call counts examine every ordered row partition, in both axes.
/// Larger rosters use bounded families of balanced counts and aspect weights
/// (at most 200 candidate partitions, rather than exponentially many). Each
/// candidate costs O(n + r log r) for n streams and r rows.
fn partitions(aspects: &[f64]) -> Vec<Vec<usize>> {
    let count = aspects.len();
    if count <= EXHAUSTIVE_COUNT {
        return (0..1usize << (count - 1))
            .map(|mask| {
                let mut groups = Vec::new();
                let mut current = 1;
                for index in 0..count - 1 {
                    if mask & (1 << index) != 0 {
                        groups.push(current);
                        current = 1;
                    } else {
                        current += 1;
                    }
                }
                groups.push(current);
                groups
            })
            .collect();
    }
    let mut result = Vec::new();
    let group_counts = (1..=count.min(64)).chain(std::iter::once(count));
    for groups in group_counts {
        let mut equal = vec![count / groups; groups];
        for size in equal.iter_mut().take(count % groups) {
            *size += 1;
        }
        result.push(equal.clone());
        equal.reverse();
        result.push(equal);
        let total: f64 = aspects.iter().sum();
        let mut balanced = Vec::with_capacity(groups);
        let mut used = 0;
        let mut weight = 0.;
        for group in 1..groups {
            let target = total * group as f64 / groups as f64;
            let start = used;
            while used < count - (groups - group) {
                if used > start && (weight - target).abs() < (weight + aspects[used] - target).abs()
                {
                    break;
                }
                weight += aspects[used];
                used += 1;
            }
            balanced.push(used - start);
        }
        balanced.push(count - used);
        result.push(balanced);
    }
    result
}

/// Rows initially fill their width, with one common height within each row.
/// If their summed heights exceed the stage, shrink all rows uniformly while
/// preserving fixed gaps. Transposing this gives independent-width columns,
/// which is particularly effective for a portrait beside landscape streams.
fn arrange(width: f64, height: f64, aspects: &[f64], partition: &[usize], gap: f64) -> Candidate {
    let mut offset = 0;
    let mut heights = Vec::with_capacity(partition.len());
    for count in partition {
        let sum: f64 = aspects[offset..offset + count].iter().sum();
        heights.push((width - gap * (count - 1) as f64) / sum);
        offset += count;
    }
    let available_height = height - gap * (partition.len() - 1) as f64;
    let scale = (available_height / heights.iter().sum::<f64>()).min(1.);
    for row_height in &mut heights {
        *row_height *= scale;
    }
    arrange_heights(width, height, aspects, partition, gap, &heights)
}

/// Mixed sources benefit from independent row heights: shrinking a full-width
/// landscape just to give a portrait more height can waste usable stage area.
/// Reserve at least the uniform layout's smallest short edge (or the compact
/// minimum when feasible), then fill width-limited row caps in bounded priority
/// orders. Two rows try both endpoints of the convex area objective exactly.
/// Equal-aspect sources retain the balanced grid candidates above.
fn nonuniform_arrangements(
    width: f64,
    height: f64,
    aspects: &[f64],
    partition: &[usize],
    gap: f64,
    uniform_shortest: f64,
) -> Vec<Candidate> {
    if partition.len() < 2
        || aspects
            .iter()
            .all(|ratio| (*ratio - aspects[0]).abs() < 0.001)
    {
        return Vec::new();
    }
    let mut offset = 0;
    let mut caps = Vec::with_capacity(partition.len());
    let mut weights = Vec::with_capacity(partition.len());
    let mut short_factors = Vec::with_capacity(partition.len());
    for count in partition {
        let row = &aspects[offset..offset + count];
        let sum: f64 = row.iter().sum();
        weights.push(sum);
        caps.push((width - gap * (count - 1) as f64) / sum);
        short_factors.push(row.iter().copied().fold(1., f64::min));
        offset += count;
    }
    let available = height - gap * (partition.len() - 1) as f64;
    let feasible_shortest = caps
        .iter()
        .zip(&short_factors)
        .map(|(cap, factor)| cap * factor)
        .fold(
            available / short_factors.iter().map(|factor| 1. / factor).sum::<f64>(),
            f64::min,
        );
    let floor = uniform_shortest.max(MIN_SHORT_EDGE.min(feasible_shortest));
    let baseline: Vec<_> = short_factors
        .iter()
        .zip(&caps)
        .map(|(factor, cap)| (floor / factor).min(*cap))
        .collect();
    let free = (available - baseline.iter().sum::<f64>()).max(0.);
    let orders = if partition.len() == 2 {
        vec![vec![0, 1], vec![1, 0]]
    } else {
        (0..3)
            .map(|priority| {
                // Gradient, average potential gradient, and natural row height
                // cover convex endpoints without enumerating row permutations.
                let mut order: Vec<_> = (0..partition.len()).collect();
                order.sort_by(|left, right| {
                    let score = |index: usize| match priority {
                        0 => weights[index] * baseline[index],
                        1 => weights[index] * (baseline[index] + caps[index]),
                        _ => caps[index],
                    };
                    score(*right).total_cmp(&score(*left)).then(left.cmp(right))
                });
                order
            })
            .collect()
    };
    let mut result = Vec::new();
    for order in orders {
        let mut heights = baseline.clone();
        let mut remaining = free;
        for index in order {
            let extra = (caps[index] - heights[index]).max(0.).min(remaining);
            heights[index] += extra;
            remaining -= extra;
        }
        result.push(arrange_heights(
            width, height, aspects, partition, gap, &heights,
        ));
    }
    result
}

fn arrange_heights(
    width: f64,
    height: f64,
    aspects: &[f64],
    partition: &[usize],
    gap: f64,
    heights: &[f64],
) -> Candidate {
    let content_height = heights.iter().sum::<f64>() + gap * (partition.len() - 1) as f64;
    let mut y = (height - content_height).max(0.) / 2.;
    let mut offset = 0;
    let mut tiles = Vec::with_capacity(aspects.len());
    let mut area = 0.;
    let mut shortest = f64::INFINITY;
    for (count, tile_height) in partition.iter().zip(heights.iter().copied()) {
        let row = &aspects[offset..offset + count];
        let row_width = row.iter().sum::<f64>() * tile_height + gap * (count - 1) as f64;
        let mut x = (width - row_width).max(0.) / 2.;
        for ratio in row {
            let tile_width = tile_height * ratio;
            tiles.push(TileRect {
                x: x as f32,
                y: y as f32,
                width: tile_width as f32,
                height: tile_height as f32,
            });
            area += tile_width * tile_height;
            shortest = shortest.min(tile_width.min(tile_height));
            x += tile_width + gap;
        }
        offset += count;
        y += tile_height + gap;
    }
    Candidate {
        tiles,
        area,
        shortest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_geometry(width: f32, height: f32, ratios: &[f32], tiles: &[TileRect]) {
        assert_eq!(tiles.len(), ratios.len());
        for (index, tile) in tiles.iter().enumerate() {
            assert!(
                tile.x.is_finite()
                    && tile.y.is_finite()
                    && tile.width.is_finite()
                    && tile.height.is_finite()
            );
            assert!(tile.width > 0. && tile.height > 0.);
            assert!(
                tile.x >= -0.001
                    && tile.y >= -0.001
                    && tile.x + tile.width <= width + 0.01
                    && tile.y + tile.height <= height + 0.01,
                "{tile:?} in {width}x{height}"
            );
            if tiles.len() > 1 {
                assert!(
                    (tile.width as f64 / tile.height as f64 - aspect(ratios[index])).abs() < 0.001
                );
            }
            for other in &tiles[..index] {
                assert!(
                    tile.x + tile.width <= other.x + 0.01
                        || other.x + other.width <= tile.x + 0.01
                        || tile.y + tile.height <= other.y + 0.01
                        || other.y + other.height <= tile.y + 0.01,
                    "overlap {tile:?} / {other:?}"
                );
            }
        }
    }

    #[test]
    fn one_stream_uses_full_actual_stage_and_empty_or_invalid_stages_are_safe() {
        assert_eq!(
            tiled_layout(933., 527., &[16. / 9.], 12.),
            vec![TileRect {
                x: 0.,
                y: 0.,
                width: 933.,
                height: 527.
            }]
        );
        assert!(tiled_layout(100., 100., &[], 12.).is_empty());
        for (width, height) in [
            (0., 100.),
            (-1., 100.),
            (f32::NAN, 100.),
            (100., f32::INFINITY),
        ] {
            assert!(tiled_layout(width, height, &[1., 1.], 12.).is_empty());
        }
    }

    #[test]
    fn wide_stage_places_landscapes_beside_each_other_and_tall_stage_stacks_them() {
        let ratios = [16. / 9.; 2];
        let wide = tiled_layout(1800., 900., &ratios, 12.);
        assert_geometry(1800., 900., &ratios, &wide);
        assert!((wide[0].y - wide[1].y).abs() < 0.01);
        assert!(wide[1].x > wide[0].x);
        let tall = tiled_layout(900., 1400., &ratios, 12.);
        assert_geometry(900., 1400., &ratios, &tall);
        assert!(tall[1].y > tall[0].y);
        assert!((tall[0].x - tall[1].x).abs() < 0.01);
    }

    #[test]
    fn mixed_portrait_and_landscapes_use_actual_pixels_instead_of_fixed_cells() {
        let ratios = [9. / 16., 16. / 9., 16. / 9.];
        let tiles = tiled_layout(1200., 800., &ratios, 12.);
        assert_geometry(1200., 800., &ratios, &tiles);
        let area: f32 = tiles.iter().map(|tile| tile.width * tile.height).sum();
        assert!(area > 1200. * 800. * 0.90, "{tiles:?} area={area}");
        assert!(tiles[0].height > tiles[1].height * 1.9);
    }

    #[test]
    fn narrow_mixed_stage_gives_landscape_full_width_instead_of_uniform_scaling() {
        let ratios = [16. / 9., 9. / 16.];
        let tiles = tiled_layout(562., 744., &ratios, 12.);
        assert_geometry(562., 744., &ratios, &tiles);
        let uniform = arrange(
            562.,
            744.,
            &ratios
                .iter()
                .map(|value| aspect(*value))
                .collect::<Vec<_>>(),
            &[1, 1],
            12.,
        );
        let area: f64 = tiles
            .iter()
            .map(|tile| f64::from(tile.width) * f64::from(tile.height))
            .sum();
        assert!(
            area > uniform.area * 1.15,
            "{tiles:?}, area={area}, old={}",
            uniform.area
        );
        assert!((tiles[0].width - 562.).abs() < 0.01);
        assert!(tiles[0].height > 300.);
        assert!(tiles[1].height > 400. && tiles[1].height < 430.);
        assert!(
            tiles
                .iter()
                .all(|tile| f64::from(tile.width.min(tile.height)) + 0.001 >= uniform.shortest)
        );
    }

    #[test]
    fn nonuniform_rows_never_shrink_below_an_already_legible_uniform_short_edge() {
        let ratios = [16. / 9., 9. / 16., 4. / 3., 1., 16. / 9., 9. / 16.];
        let aspects: Vec<_> = ratios.iter().map(|value| aspect(*value)).collect();
        let partition = [2, 2, 2];
        let uniform = arrange(1600., 1000., &aspects, &partition, 12.);
        assert!(uniform.shortest > MIN_SHORT_EDGE);
        for candidate in
            nonuniform_arrangements(1600., 1000., &aspects, &partition, 12., uniform.shortest)
        {
            assert!(candidate.shortest + 0.001 >= uniform.shortest);
            assert_geometry(1600., 1000., &ratios, &candidate.tiles);
        }
        assert!(
            nonuniform_arrangements(1600., 1000., &[16. / 9.; 6], &partition, 12., 100.).is_empty()
        );
    }

    #[test]
    fn increasing_counts_and_resizing_never_overlap_or_leave_bounds() {
        for count in 2..=96 {
            let ratios: Vec<_> = (0..count)
                .map(|index| [16. / 9., 9. / 16., 1., 4. / 3.][index % 4])
                .collect();
            for (width, height) in [(1440., 900.), (400., 900.), (900., 260.), (3., 2.)] {
                let tiles = tiled_layout(width, height, &ratios, 12.);
                assert_geometry(width, height, &ratios, &tiles);
            }
        }
    }

    #[test]
    fn malformed_aspects_and_gap_values_remain_bounded_and_deterministic() {
        let ratios = [0., f32::NAN, -1., f32::INFINITY, 1e-30, 1e30];
        for gap in [f32::NAN, -12., 12., 1e30] {
            let first = tiled_layout(600., 400., &ratios, gap);
            assert_geometry(600., 400., &ratios, &first);
            assert_eq!(first, tiled_layout(600., 400., &ratios, gap));
        }
    }
}
