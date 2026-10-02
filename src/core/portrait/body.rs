//! Body shape: where a person's shoulders, neck, waist, hips and limbs lie,
//! from the pose landmarks (the joints) and the part model's silhouette (the
//! edges around them).

use crate::core::ai::body_parts::BodyLabels;
use crate::core::ai::pose::{self, Pose};

const BACKGROUND: u8 = 0;
const FACE_NECK: u8 = 3;
const HAIR: u8 = 4;
const TORSO: u8 = 22;
/// Arm radius as a share of the torso's length (shoulders to hips), clothes
/// included.
const ARM_RADIUS: f32 = 0.12;
/// Where the waist is looked for, as shares of the way from the shoulder
/// line to the hip joints.
const WAIST_FROM: f32 = 0.55;
const WAIST_TO: f32 = 0.78;
/// The usual waist height on that scale, and how much a level away from it
/// must be narrower (in shoulder widths per unit) to be taken instead.
const WAIST_AT: f32 = 0.64;
const WAIST_PULL: f32 = 1.5;
/// The narrowest a waist can be, in shoulder-joint widths.
const MIN_WAIST: f32 = 0.5;
/// How narrow and wide a neck can be, in shoulder-joint widths; thinner
/// strips of neck skin show between a collar and the chin.
const NECK: (f32, f32) = (0.2, 0.7);

/// The person's pixels in the part model's grid: every labelled cell joined
/// to the seed, so people around them are left out.
pub struct Silhouette {
    labels: BodyLabels,
    mask: Vec<bool>,
}

impl Silhouette {
    /// The person holding image point `seed`, or `None` when the model sees
    /// nobody near it.
    pub fn new(labels: BodyLabels, seed: [f32; 2]) -> Option<Self> {
        let (w, h) = BodyLabels::SIZE;
        let [u, v] = labels.to_grid(seed[0], seed[1]);
        let start = (u as isize, v as isize);
        // The nearest labelled cell within a few cells of the seed.
        let found = (0..6isize).find_map(|r| {
            (-r..=r)
                .flat_map(|dy| (-r..=r).map(move |dx| (start.0 + dx, start.1 + dy)))
                .find(|&(x, y)| {
                    x >= 0
                        && y >= 0
                        && (x as usize) < w
                        && (y as usize) < h
                        && labels.label(x as usize, y as usize) != BACKGROUND
                })
        })?;
        let mut mask = vec![false; w * h];
        let mut stack = vec![(found.0 as usize, found.1 as usize)];
        mask[found.1 as usize * w + found.0 as usize] = true;
        while let Some((x, y)) = stack.pop() {
            let mut visit = |nx: usize, ny: usize| {
                let i = ny * w + nx;
                if !mask[i] && labels.label(nx, ny) != BACKGROUND {
                    mask[i] = true;
                    stack.push((nx, ny));
                }
            };
            if x > 0 {
                visit(x - 1, y);
            }
            if y > 0 {
                visit(x, y - 1);
            }
            if x + 1 < w {
                visit(x + 1, y);
            }
            if y + 1 < h {
                visit(x, y + 1);
            }
        }
        Some(Self { labels, mask })
    }

    /// The person's label at image point (x, y); `None` off the person.
    pub fn label_at(&self, x: f32, y: f32) -> Option<u8> {
        let (w, h) = BodyLabels::SIZE;
        let [u, v] = self.labels.to_grid(x, y);
        if u < 0.0 || v < 0.0 || u >= w as f32 || v >= h as f32 {
            return None;
        }
        let i = v as usize * w + u as usize;
        self.mask[i].then(|| self.labels.label(u as usize, v as usize))
    }

    /// Image pixels per grid cell.
    pub fn scale(&self) -> f32 {
        self.labels.scale()
    }

    /// How far from `from` along the unit vector `dir` the person goes on,
    /// up to `limit`, while `keep(label, point)` holds.
    fn reach(
        &self,
        from: [f32; 2],
        dir: [f32; 2],
        limit: f32,
        keep: impl Fn(u8, [f32; 2]) -> bool,
    ) -> f32 {
        let step = (self.scale() * 0.5).max(0.5);
        let mut d = 0.0;
        while d < limit {
            let p = [from[0] + dir[0] * (d + step), from[1] + dir[1] * (d + step)];
            match self.label_at(p[0], p[1]) {
                Some(label) if keep(label, p) => d += step,
                _ => break,
            }
        }
        d.min(limit)
    }
}

/// A line across a part of the body: its middle and its two edges, the
/// first toward -across (the image left for someone facing the camera).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub centre: [f32; 2],
    pub start: [f32; 2],
    pub end: [f32; 2],
}

impl Span {
    pub fn width(&self) -> f32 {
        (self.end[0] - self.start[0]).hypot(self.end[1] - self.start[1])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimbKind {
    UpperArm,
    Forearm,
    Thigh,
    Shin,
}

/// One limb segment from joint to joint, and the span across its middle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limb {
    pub kind: LimbKind,
    pub from: [f32; 2],
    pub to: [f32; 2],
    pub across: Option<Span>,
}

/// What the body sliders need to know about one person.
#[derive(Clone, Debug, PartialEq)]
pub struct BodyShape {
    /// Unit vectors down the body (shoulders to hips) and across it (from
    /// the person's right shoulder to their left).
    pub down: [f32; 2],
    pub across: [f32; 2],
    /// Outer edges of the shoulders, through the shoulder joints.
    pub shoulders: Span,
    pub neck: Option<Span>,
    pub waist: Option<Span>,
    pub hips: Option<Span>,
    pub limbs: Vec<Limb>,
}

fn add(p: [f32; 2], v: [f32; 2], k: f32) -> [f32; 2] {
    [p[0] + v[0] * k, p[1] + v[1] * k]
}

fn mid(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    (b[0] - a[0]).hypot(b[1] - a[1])
}

/// Distance from `p` to the segment `a`-`b`.
fn to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length = dx * dx + dy * dy;
    let t = if length > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    distance(p, [a[0] + dx * t, a[1] + dy * t])
}

/// Measures the person: `pose` places the joints, `silhouette` the edges,
/// `chin` (the face mesh's chin point) tops the neck. `None` when the
/// shoulders are not seen.
pub fn measure(pose: &Pose, silhouette: &Silhouette, chin: [f32; 2]) -> Option<BodyShape> {
    let seen = |k: usize| pose.seen(k);
    if !seen(pose::LEFT_SHOULDER) || !seen(pose::RIGHT_SHOULDER) {
        return None;
    }
    let (left_shoulder, right_shoulder) =
        (pose.at(pose::LEFT_SHOULDER), pose.at(pose::RIGHT_SHOULDER));
    let shoulder_mid = mid(left_shoulder, right_shoulder);
    let shoulder_width = distance(left_shoulder, right_shoulder).max(1.0);
    let across = [
        (left_shoulder[0] - right_shoulder[0]) / shoulder_width,
        (left_shoulder[1] - right_shoulder[1]) / shoulder_width,
    ];
    // Down the body: perpendicular to the shoulders, toward the hips.
    let hip_points = (pose.at(pose::LEFT_HIP), pose.at(pose::RIGHT_HIP));
    let hip_mid = mid(hip_points.0, hip_points.1);
    let mut down = [-across[1], across[0]];
    if (hip_mid[0] - shoulder_mid[0]) * down[0] + (hip_mid[1] - shoulder_mid[1]) * down[1] < 0.0 {
        down = [-down[0], -down[1]];
    }
    let hips_seen = seen(pose::LEFT_HIP) && seen(pose::RIGHT_HIP);
    // Shoulders to hips; without hips, a typical torso for these shoulders.
    let torso = if hips_seen {
        distance(shoulder_mid, hip_mid).max(shoulder_width)
    } else {
        shoulder_width * 1.4
    };
    let arm_radius = ARM_RADIUS * torso;
    // Arms as joint chains, where they are seen.
    let arm = |shoulder: usize, elbow: usize, wrist: usize| -> Vec<[f32; 2]> {
        let mut chain = vec![pose.at(shoulder)];
        if seen(elbow) {
            chain.push(pose.at(elbow));
            if seen(wrist) {
                chain.push(pose.at(wrist));
            }
        }
        chain
    };
    let arms = [
        arm(pose::LEFT_SHOULDER, pose::LEFT_ELBOW, pose::LEFT_WRIST),
        arm(pose::RIGHT_SHOULDER, pose::RIGHT_ELBOW, pose::RIGHT_WRIST),
    ];
    let on_arm = |p: [f32; 2]| {
        arms.iter().any(|chain| {
            chain
                .windows(2)
                .any(|s| to_segment(p, s[0], s[1]) < arm_radius)
        })
    };
    let trunk = |label: u8, p: [f32; 2]| label != HAIR && !on_arm(p);
    let span_at = |centre: [f32; 2], limit: f32, keep: &dyn Fn(u8, [f32; 2]) -> bool| {
        let back = silhouette.reach(centre, [-across[0], -across[1]], limit, keep);
        let on = silhouette.reach(centre, across, limit, keep);
        Span {
            centre,
            start: add(centre, across, -back),
            end: add(centre, across, on),
        }
    };

    // Shoulders: from each joint outward to the edge.
    let reach = 0.5 * shoulder_width;
    let outer = |joint: [f32; 2], sign: f32| {
        let dir = [across[0] * sign, across[1] * sign];
        add(
            joint,
            dir,
            silhouette.reach(joint, dir, reach, |l, _| l != HAIR),
        )
    };
    let shoulders = Span {
        centre: shoulder_mid,
        start: outer(right_shoulder, -1.0),
        end: outer(left_shoulder, 1.0),
    };

    // Neck: its narrowest neck skin between the chin and the shoulder line
    // (lower down, the same label runs on over the chest), when wide enough
    // to be the neck.
    let below_chin = (shoulder_mid[0] - chin[0]) * down[0] + (shoulder_mid[1] - chin[1]) * down[1];
    let neck = (below_chin > 0.0)
        .then(|| {
            (0..=10)
                .map(|i| add(chin, down, below_chin * (0.15 + 0.06 * i as f32)))
                .map(|c| span_at(c, 0.5 * shoulder_width, &|l, _| l == FACE_NECK))
                .filter(|s| (NECK.0..=NECK.1).contains(&(s.width() / shoulder_width)))
                .min_by(|a, b| a.width().total_cmp(&b.width()))
        })
        .flatten();

    // Waist: the narrowest trunk in the band where waists are.
    let level = |t: f32| add(shoulder_mid, down, torso * t);
    let inside = |p: [f32; 2]| silhouette.label_at(p[0], p[1]).is_some();
    // Spans down the trunk, with how far down each lies.
    let spans = |from: f32, to: f32| {
        let steps = ((to - from) / 0.025).round() as usize;
        (0..=steps)
            .map(|i| from + (to - from) * i as f32 / steps as f32)
            .filter(|&t| inside(level(t)))
            .map(|t| (t, span_at(level(t), shoulder_width, &trunk)))
            .collect::<Vec<_>>()
    };
    // Along a straight outline the narrowest point means little, so it is
    // weighed against the usual waist height. Arms held in front of it
    // leave too little to be the waist.
    let waist_score =
        |(t, s): &(f32, Span)| s.width() / shoulder_width + WAIST_PULL * (t - WAIST_AT).abs();
    let waist = hips_seen
        .then(|| {
            spans(WAIST_FROM, WAIST_TO)
                .into_iter()
                .min_by(|a, b| waist_score(a).total_cmp(&waist_score(b)))
                .map(|(_, s)| s)
        })
        .flatten()
        .filter(|w| w.width() >= MIN_WAIST * shoulder_width);
    // Hips: the widest trunk around the hip joints, below the waist.
    let hips = hips_seen
        .then(|| {
            spans(0.9, 1.15)
                .into_iter()
                .map(|(_, s)| s)
                .max_by(|a, b| a.width().total_cmp(&b.width()))
        })
        .flatten();

    // Limbs: joint to joint, measured across the middle.
    let segments = [
        (LimbKind::UpperArm, pose::LEFT_SHOULDER, pose::LEFT_ELBOW),
        (LimbKind::UpperArm, pose::RIGHT_SHOULDER, pose::RIGHT_ELBOW),
        (LimbKind::Forearm, pose::LEFT_ELBOW, pose::LEFT_WRIST),
        (LimbKind::Forearm, pose::RIGHT_ELBOW, pose::RIGHT_WRIST),
        (LimbKind::Thigh, pose::LEFT_HIP, pose::LEFT_KNEE),
        (LimbKind::Thigh, pose::RIGHT_HIP, pose::RIGHT_KNEE),
        (LimbKind::Shin, pose::LEFT_KNEE, pose::LEFT_ANKLE),
        (LimbKind::Shin, pose::RIGHT_KNEE, pose::RIGHT_ANKLE),
    ];
    let limbs = segments
        .iter()
        .filter(|(_, a, b)| seen(*a) && seen(*b))
        .map(|&(kind, a, b)| {
            let (from, to) = (pose.at(a), pose.at(b));
            let length = distance(from, to).max(1.0);
            let dir = [(to[0] - from[0]) / length, (to[1] - from[1]) / length];
            let normal = [-dir[1], dir[0]];
            let centre = mid(from, to);
            // Bare arms stop at the torso's skin; clothed ones at a cap.
            let keep = |l: u8, _: [f32; 2]| l != HAIR && l != TORSO;
            let limit = 0.35 * length;
            let across = inside(centre).then(|| {
                let back = silhouette.reach(centre, [-normal[0], -normal[1]], limit, keep);
                let on = silhouette.reach(centre, normal, limit, keep);
                Span {
                    centre,
                    start: add(centre, normal, -back),
                    end: add(centre, normal, on),
                }
            });
            Limb {
                kind,
                from,
                to,
                across,
            }
        })
        .collect();

    Some(BodyShape {
        down,
        across,
        shoulders,
        neck,
        waist,
        hips,
        limbs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_distance_clamps_to_the_ends() {
        assert_eq!(to_segment([5.0, 3.0], [0.0, 0.0], [10.0, 0.0]), 3.0);
        assert_eq!(to_segment([-4.0, 3.0], [0.0, 0.0], [10.0, 0.0]), 5.0);
        assert_eq!(to_segment([1.0, 1.0], [2.0, 2.0], [2.0, 2.0]), 2f32.sqrt());
    }
}
