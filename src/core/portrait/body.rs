//! Body shape: where a person's shoulders, neck, waist, hips and limbs lie,
//! from the pose landmarks (the joints) and the part model's silhouette (the
//! edges around them), and the warps the body sliders make from them.

use std::collections::HashMap;

use super::analysis::{Clip, PortraitModel};
use super::geometry::{loop_points, Region, FACE_OVAL};
use super::reshape::{Control, Displacement};
use crate::core::ai::body_parts::{self, BodyLabels, Segmenter};
use crate::core::ai::pose::{self, Pose, PoseModel};

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
/// A face this large against the image (extent over height) is a close-up:
/// no body to work on.
const CLOSE_UP: f32 = 0.4;

/// Slider strengths at 100: the waist's and the limbs' half-widths drawn
/// in by these shares, the shoulders by this share of their half-width,
/// the neck lengthened and the legs below the hips stretched by these.
const WAIST_STRENGTH: f32 = 0.15;
const ARM_STRENGTH: f32 = 0.18;
const LEG_STRENGTH: f32 = 0.12;
const SHOULDER_STRENGTH: f32 = 0.1;
const NECK_STRENGTH: f32 = 0.3;
const LEG_LENGTH_STRENGTH: f32 = 0.12;

/// Why a search along the body stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    /// The person ends there: an outline that can move.
    Off,
    /// Something else of the person starts there (an arm, hair).
    Blocked,
    /// The search ran its full length.
    Limit,
}

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

    /// Image bounds of the person: [x0, y0, x1, y1].
    pub fn bounds(&self) -> [f32; 4] {
        let (w, _) = BodyLabels::SIZE;
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for (i, _) in self.mask.iter().enumerate().filter(|(_, on)| **on) {
            let (u, v) = ((i % w) as f32, (i / w) as f32);
            for [x, y] in [
                self.labels.to_image(u, v),
                self.labels.to_image(u + 1.0, v + 1.0),
            ] {
                b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
        }
        b
    }

    /// How far from `from` along the unit vector `dir` the person goes on,
    /// up to `limit`, while `keep(label, point)` holds, and why it stopped.
    fn reach(
        &self,
        from: [f32; 2],
        dir: [f32; 2],
        limit: f32,
        keep: impl Fn(u8, [f32; 2]) -> bool,
    ) -> (f32, Stop) {
        let step = (self.scale() * 0.5).max(0.5);
        let mut d = 0.0;
        while d < limit {
            let p = [from[0] + dir[0] * (d + step), from[1] + dir[1] * (d + step)];
            match self.label_at(p[0], p[1]) {
                Some(label) if keep(label, p) => d += step,
                Some(_) => return (d, Stop::Blocked),
                None => return (d, Stop::Off),
            }
        }
        (limit, Stop::Limit)
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
        distance(self.start, self.end)
    }

    fn edge(&self, side: usize) -> [f32; 2] {
        if side == 0 {
            self.start
        } else {
            self.end
        }
    }
}

/// A span and why each edge ends where it does.
#[derive(Clone, Copy, Debug)]
struct Edges {
    span: Span,
    stops: [Stop; 2],
}

/// A span from `centre` both ways along the unit vector `dir`.
fn edges(
    silhouette: &Silhouette,
    centre: [f32; 2],
    dir: [f32; 2],
    limit: f32,
    keep: &dyn Fn(u8, [f32; 2]) -> bool,
) -> Edges {
    let back = silhouette.reach(centre, [-dir[0], -dir[1]], limit, keep);
    let on = silhouette.reach(centre, dir, limit, keep);
    Edges {
        span: Span {
            centre,
            start: add(centre, dir, -back.0),
            end: add(centre, dir, on.0),
        },
        stops: [back.1, on.1],
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
    /// How far down the waist lies, in torso lengths below the shoulders.
    pub waist_level: Option<f32>,
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

fn dot(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
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

/// The trunk's frame from the pose: shoulder line, axis, torso length and
/// the arms as joint chains.
struct Trunk<'a> {
    pose: &'a Pose,
    silhouette: &'a Silhouette,
    across: [f32; 2],
    down: [f32; 2],
    shoulder_mid: [f32; 2],
    shoulder_width: f32,
    torso: f32,
    hips_seen: bool,
    arms: [Vec<[f32; 2]>; 2],
    arm_radius: f32,
}

impl<'a> Trunk<'a> {
    fn new(pose: &'a Pose, silhouette: &'a Silhouette) -> Option<Self> {
        let seen = |k: usize| pose.seen(k);
        if !seen(pose::LEFT_SHOULDER) || !seen(pose::RIGHT_SHOULDER) {
            return None;
        }
        let (left, right) = (pose.at(pose::LEFT_SHOULDER), pose.at(pose::RIGHT_SHOULDER));
        let shoulder_mid = mid(left, right);
        let shoulder_width = distance(left, right).max(1.0);
        let across = [
            (left[0] - right[0]) / shoulder_width,
            (left[1] - right[1]) / shoulder_width,
        ];
        let hip_mid = mid(pose.at(pose::LEFT_HIP), pose.at(pose::RIGHT_HIP));
        // Down the body: perpendicular to the shoulders, toward the hips.
        let mut down = [-across[1], across[0]];
        if dot(sub(hip_mid, shoulder_mid), down) < 0.0 {
            down = [-down[0], -down[1]];
        }
        let hips_seen = seen(pose::LEFT_HIP) && seen(pose::RIGHT_HIP);
        // Shoulders to hips; without hips, a typical torso for these shoulders.
        let torso = if hips_seen {
            distance(shoulder_mid, hip_mid).max(shoulder_width)
        } else {
            shoulder_width * 1.4
        };
        // The subject's right arm first: it hangs on the -across side.
        let arm = |shoulder: usize, elbow: usize, wrist: usize| {
            let mut chain = vec![pose.at(shoulder)];
            if seen(elbow) {
                chain.push(pose.at(elbow));
                if seen(wrist) {
                    chain.push(pose.at(wrist));
                }
            }
            chain
        };
        Some(Self {
            pose,
            silhouette,
            across,
            down,
            shoulder_mid,
            shoulder_width,
            torso,
            hips_seen,
            arms: [
                arm(pose::RIGHT_SHOULDER, pose::RIGHT_ELBOW, pose::RIGHT_WRIST),
                arm(pose::LEFT_SHOULDER, pose::LEFT_ELBOW, pose::LEFT_WRIST),
            ],
            arm_radius: ARM_RADIUS * torso,
        })
    }

    /// The point on the axis `t` torso lengths below the shoulder line.
    fn level(&self, t: f32) -> [f32; 2] {
        add(self.shoulder_mid, self.down, self.torso * t)
    }

    /// How far down `p` lies, in torso lengths.
    fn level_of(&self, p: [f32; 2]) -> f32 {
        dot(sub(p, self.shoulder_mid), self.down) / self.torso
    }

    fn on_arm(&self, p: [f32; 2]) -> bool {
        self.arms.iter().any(|chain| {
            chain
                .windows(2)
                .any(|s| to_segment(p, s[0], s[1]) < self.arm_radius)
        })
    }

    fn inside(&self, p: [f32; 2]) -> bool {
        self.silhouette.label_at(p[0], p[1]).is_some()
    }

    /// The trunk across level `t`, arms and hair left out.
    fn span(&self, t: f32) -> Option<Edges> {
        let centre = self.level(t);
        self.inside(centre).then(|| {
            edges(
                self.silhouette,
                centre,
                self.across,
                self.shoulder_width,
                &|label, p| label != HAIR && !self.on_arm(p),
            )
        })
    }

    /// Levels from `from` to `to` in steps of about `step`.
    fn levels(from: f32, to: f32, step: f32) -> impl Iterator<Item = f32> {
        let steps = ((to - from) / step).round().max(1.0) as usize;
        (0..=steps).map(move |i| from + (to - from) * i as f32 / steps as f32)
    }

    /// Across one limb segment at `s` (0 at `from`, 1 at `to`): the limb's
    /// own pixels, stopping at the torso's skin and at a cap for clothed
    /// limbs lying against the body.
    fn limb_span(&self, from: [f32; 2], to: [f32; 2], s: f32) -> Option<Edges> {
        let length = distance(from, to).max(1.0);
        let dir = [(to[0] - from[0]) / length, (to[1] - from[1]) / length];
        let centre = add(from, dir, length * s);
        self.inside(centre).then(|| {
            edges(
                self.silhouette,
                centre,
                [-dir[1], dir[0]],
                0.35 * length,
                &|l, _| l != HAIR && l != TORSO,
            )
        })
    }

    /// The pose's limb segments with both joints seen.
    fn segments(&self) -> Vec<(LimbKind, [f32; 2], [f32; 2])> {
        [
            (LimbKind::UpperArm, pose::LEFT_SHOULDER, pose::LEFT_ELBOW),
            (LimbKind::UpperArm, pose::RIGHT_SHOULDER, pose::RIGHT_ELBOW),
            (LimbKind::Forearm, pose::LEFT_ELBOW, pose::LEFT_WRIST),
            (LimbKind::Forearm, pose::RIGHT_ELBOW, pose::RIGHT_WRIST),
            (LimbKind::Thigh, pose::LEFT_HIP, pose::LEFT_KNEE),
            (LimbKind::Thigh, pose::RIGHT_HIP, pose::RIGHT_KNEE),
            (LimbKind::Shin, pose::LEFT_KNEE, pose::LEFT_ANKLE),
            (LimbKind::Shin, pose::RIGHT_KNEE, pose::RIGHT_ANKLE),
        ]
        .into_iter()
        .filter(|(_, a, b)| self.pose.seen(*a) && self.pose.seen(*b))
        .map(|(kind, a, b)| (kind, self.pose.at(a), self.pose.at(b)))
        .collect()
    }
}

/// Measures the person: `pose` places the joints, `silhouette` the edges,
/// `chin` (the face mesh's chin point) tops the neck. `None` when the
/// shoulders are not seen.
pub fn measure(pose: &Pose, silhouette: &Silhouette, chin: [f32; 2]) -> Option<BodyShape> {
    let trunk = Trunk::new(pose, silhouette)?;
    let (across, down, shoulder_width) = (trunk.across, trunk.down, trunk.shoulder_width);
    let (left, right) = (pose.at(pose::LEFT_SHOULDER), pose.at(pose::RIGHT_SHOULDER));

    // Shoulders: from each joint outward to the edge.
    let outer = |joint: [f32; 2], sign: f32| {
        let dir = [across[0] * sign, across[1] * sign];
        let (d, _) = silhouette.reach(joint, dir, 0.5 * shoulder_width, |l, _| l != HAIR);
        add(joint, dir, d)
    };
    let shoulders = Span {
        centre: trunk.shoulder_mid,
        start: outer(right, -1.0),
        end: outer(left, 1.0),
    };

    // Neck: its narrowest neck skin between the chin and the shoulder line
    // (lower down, the same label runs on over the chest), when wide enough
    // to be the neck.
    let below_chin = dot(sub(trunk.shoulder_mid, chin), down);
    let neck = (below_chin > 0.0)
        .then(|| {
            (0..=10)
                .map(|i| add(chin, down, below_chin * (0.15 + 0.06 * i as f32)))
                .map(|c| {
                    edges(silhouette, c, across, 0.5 * shoulder_width, &|l, _| {
                        l == FACE_NECK
                    })
                    .span
                })
                .filter(|s| (NECK.0..=NECK.1).contains(&(s.width() / shoulder_width)))
                .min_by(|a, b| a.width().total_cmp(&b.width()))
        })
        .flatten();

    // Waist: the narrowest trunk in the band where waists are. Along a
    // straight outline the narrowest point means little, so it is weighed
    // against the usual waist height. Arms held in front of it leave too
    // little to be the waist.
    let spans = |from: f32, to: f32| {
        Trunk::levels(from, to, 0.025)
            .filter_map(|t| trunk.span(t).map(|e| (t, e.span)))
            .collect::<Vec<_>>()
    };
    let waist_score =
        |(t, s): &(f32, Span)| s.width() / shoulder_width + WAIST_PULL * (t - WAIST_AT).abs();
    let waist = trunk
        .hips_seen
        .then(|| {
            spans(WAIST_FROM, WAIST_TO)
                .into_iter()
                .min_by(|a, b| waist_score(a).total_cmp(&waist_score(b)))
        })
        .flatten()
        .filter(|(_, w)| w.width() >= MIN_WAIST * shoulder_width);
    // Hips: the widest trunk around the hip joints, below the waist.
    let hips = trunk
        .hips_seen
        .then(|| {
            spans(0.9, 1.15)
                .into_iter()
                .map(|(_, s)| s)
                .max_by(|a, b| a.width().total_cmp(&b.width()))
        })
        .flatten();

    // Limbs: joint to joint, measured across the middle.
    let limbs = trunk
        .segments()
        .into_iter()
        .map(|(kind, from, to)| Limb {
            kind,
            from,
            to,
            across: trunk.limb_span(from, to, 0.5).map(|e| e.span),
        })
        .collect();

    Some(BodyShape {
        down,
        across,
        shoulders,
        neck,
        waist: waist.map(|(_, s)| s),
        waist_level: waist.map(|(t, _)| t),
        hips,
        limbs,
    })
}

/// One person's body, ready for the sliders.
pub struct BodyModel {
    pub pose: Pose,
    pub silhouette: Silhouette,
    pub shape: BodyShape,
}

/// The bodies below `model`'s faces (`None` where a face has none to work
/// on), from the pose and part models. Errors when either model is missing.
pub fn analyze_bodies(
    rgba: &[u8],
    model: &PortraitModel,
    prefer_gpu: bool,
) -> Result<Vec<Option<BodyModel>>, String> {
    if pose::model_path().is_none() {
        return Err("thiếu model khung xương (models\\pose)".to_string());
    }
    if body_parts::model_path().is_none() {
        return Err("thiếu model tách vùng Sapiens2".to_string());
    }
    let (width, height) = (model.width, model.height);
    let frame = model.clip.as_ref().map(Clip::bounds);
    let mut poser = PoseModel::load(prefer_gpu)?;
    let mut segmenter: Option<Segmenter> = None;
    let mut bodies = Vec::with_capacity(model.faces.len());
    for face in &model.faces {
        if face.extent > CLOSE_UP * height as f32 {
            bodies.push(None);
            continue;
        }
        let Some(pose) = poser.detect(rgba, width, height, &face.mesh)? else {
            bodies.push(None);
            continue;
        };
        let seed = mid(pose.at(pose::LEFT_SHOULDER), pose.at(pose::RIGHT_SHOULDER));
        let segmenter = match &mut segmenter {
            Some(s) => s,
            None => segmenter.insert(Segmenter::load(prefer_gpu)?),
        };
        let labels = segmenter
            .segment_body(rgba, width, height, &face.mesh, frame, 1)?
            .into_iter()
            .next();
        let points = &face.mesh.points;
        let chin = [points[152][0], points[152][1]];
        let body = labels
            .and_then(|labels| Silhouette::new(labels, seed))
            .and_then(|silhouette| {
                let shape = measure(&pose, &silhouette, chin)?;
                Some(BodyModel {
                    pose,
                    silhouette,
                    shape,
                })
            });
        bodies.push(body);
    }
    Ok(bodies)
}

/// Body sliders, -100..100 except `leg_length` (0..100); 0 = as shot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BodySliders {
    /// Draw the waist in (positive) or out.
    pub waist: f32,
    /// Narrow (positive) or broaden the shoulders.
    pub shoulders: f32,
    /// Lengthen (positive) or shorten the neck.
    pub neck: f32,
    /// Slim (positive) or fill out the arms.
    pub arms: f32,
    /// Slim (positive) or fill out the legs.
    pub legs: f32,
    /// Lengthen the legs.
    pub leg_length: f32,
}

impl BodySliders {
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }

    /// Whether anything besides the leg length moves.
    fn reshapes(&self) -> bool {
        Self {
            leg_length: 0.0,
            ..*self
        } != Self::default()
    }
}

fn unit(v: f32) -> f32 {
    v.clamp(-100.0, 100.0) / 100.0
}

/// Control points gathered by position: moves from several sliders at one
/// point add up, so no point gets two different targets.
#[derive(Default)]
struct Moves {
    points: HashMap<(i32, i32), ([f32; 2], [f32; 2])>,
    order: Vec<(i32, i32)>,
}

impl Moves {
    fn key(p: [f32; 2]) -> (i32, i32) {
        ((p[0] / 2.0).round() as i32, (p[1] / 2.0).round() as i32)
    }

    fn shift(&mut self, at: [f32; 2], by: [f32; 2]) {
        let key = Self::key(at);
        if !self.points.contains_key(&key) {
            self.order.push(key);
            self.points.insert(key, (at, [0.0; 2]));
        }
        if let Some(entry) = self.points.get_mut(&key) {
            entry.1[0] += by[0];
            entry.1[1] += by[1];
        }
    }

    fn hold(&mut self, at: [f32; 2]) {
        self.shift(at, [0.0; 2]);
    }

    fn controls(&self) -> Vec<Control> {
        self.order
            .iter()
            .map(|k| {
                let (from, by) = self.points[k];
                Control {
                    from,
                    to: [from[0] + by[0], from[1] + by[1]],
                }
            })
            .collect()
    }
}

/// The warp for one person's body sliders (all but the leg length): the
/// moved outlines, the axis and the parts they leave held, and a ring of
/// anchors around the person. `face` is the person's face mesh, moved with
/// the neck. `None` when nothing moves.
pub fn body_field(
    body: &BodyModel,
    face: &[[f32; 3]],
    sliders: &BodySliders,
    width: u32,
    height: u32,
) -> Option<Displacement> {
    if !sliders.reshapes() {
        return None;
    }
    let trunk = Trunk::new(&body.pose, &body.silhouette)?;
    let shape = &body.shape;
    let (across, down) = (trunk.across, trunk.down);
    let mut moves = Moves::default();
    let step = trunk.torso * 0.025;

    // Waist: both sides in toward the axis, most at the waist line, fading
    // up to the chest and down to the hips. An arm against the waist goes
    // in with it; one clear of it holds.
    let waist = unit(sliders.waist) * WAIST_STRENGTH;
    let mut waist_shift: Vec<(f32, [Option<[f32; 2]>; 2])> = Vec::new();
    if let (true, Some(tw)) = (waist != 0.0, shape.waist_level) {
        let (above, below) = (0.3, 0.35);
        for t in Trunk::levels(tw - above, tw + below, 0.02) {
            let Some(e) = trunk.span(t) else {
                continue;
            };
            let weight = if t < tw {
                1.0 - smoothstep(0.0, above, tw - t)
            } else {
                1.0 - smoothstep(0.0, below, t - tw)
            };
            let mut shifts = [None; 2];
            for (side, shift) in shifts.iter_mut().enumerate() {
                let edge = e.span.edge(side);
                if e.stops[side] == Stop::Limit {
                    moves.hold(edge);
                    continue;
                }
                let by = sub(e.span.centre, edge).map(|v| v * waist * weight);
                moves.shift(edge, by);
                if e.stops[side] == Stop::Blocked {
                    *shift = Some(by);
                }
            }
            moves.hold(e.span.centre);
            waist_shift.push((t, shifts));
        }
    }
    // Arms: where one blocked the waist's edge, it moves with that edge.
    for (side, chain) in trunk.arms.iter().enumerate() {
        for s in chain.windows(2) {
            let length = distance(s[0], s[1]);
            let pieces = (length / step).ceil().max(1.0) as usize;
            for i in 0..=pieces {
                let p = add(s[0], sub(s[1], s[0]), i as f32 / pieces as f32);
                let t = trunk.level_of(p);
                let near = waist_shift
                    .iter()
                    .min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs()))
                    .filter(|(level, _)| (level - t).abs() < 0.03);
                match near.and_then(|(_, shifts)| shifts[side]) {
                    Some(by) => moves.shift(p, by),
                    None if sliders.arms == 0.0 && sliders.shoulders == 0.0 => moves.hold(p),
                    None => {}
                }
            }
        }
    }

    // Shoulders: each outer edge and the line in from the neck move across,
    // the arm going along (less toward the wrist).
    let shoulders = unit(sliders.shoulders) * SHOULDER_STRENGTH;
    if shoulders != 0.0 {
        let half = shape.shoulders.width() * 0.5;
        let neck_half = shape
            .neck
            .map_or(0.18 * trunk.shoulder_width, |n| n.width() * 0.5);
        for (side, chain) in trunk.arms.iter().enumerate() {
            let sign = if side == 0 { 1.0 } else { -1.0 };
            let inward = [across[0] * sign, across[1] * sign];
            let by = inward.map(|v| v * half * shoulders);
            let edge = shape.shoulders.edge(side);
            let neck_edge = add(trunk.shoulder_mid, inward, -neck_half);
            for i in 0..=8 {
                let f = i as f32 / 8.0;
                let p = add(neck_edge, sub(edge, neck_edge), f);
                moves.shift(p, by.map(|v| v * f * f));
            }
            for (k, &joint) in chain.iter().enumerate() {
                let share = [1.0, 0.75, 0.5][k];
                moves.shift(joint, by.map(|v| v * share));
            }
        }
    }

    // Neck: the head rises (or sinks), the neck stretching between the chin
    // and the shoulders.
    let neck = unit(sliders.neck) * NECK_STRENGTH;
    if neck != 0.0 && face.len() > 152 {
        let chin = [face[152][0], face[152][1]];
        let length = dot(sub(trunk.shoulder_mid, chin), down).max(1.0);
        let up = [-down[0], -down[1]];
        let by = up.map(|v| v * length * neck);
        for p in loop_points(face, &FACE_OVAL) {
            moves.shift(p, by);
        }
        // The hair above and beside the face rises with it.
        let top = [face[10][0], face[10][1]];
        let (d, _) = body.silhouette.reach(top, up, length * 3.0, |_, _| true);
        moves.shift(add(top, up, d), by);
        let centre = mid(top, chin);
        for sign in [-1.0f32, 1.0] {
            let dir = [across[0] * sign, across[1] * sign];
            let (d, _) = body
                .silhouette
                .reach(centre, dir, trunk.shoulder_width, |_, _| true);
            moves.shift(add(centre, dir, d), by);
        }
        for i in 1..10 {
            let f = i as f32 / 10.0;
            let centre = add(chin, down, length * f);
            let e = edges(
                &body.silhouette,
                centre,
                across,
                0.5 * trunk.shoulder_width,
                &|l, _| l == FACE_NECK,
            );
            let by = by.map(|v| v * (1.0 - f));
            moves.shift(e.span.start, by);
            moves.shift(e.span.end, by);
            moves.shift(centre, by);
        }
        moves.hold(shape.shoulders.start);
        moves.hold(shape.shoulders.end);
    } else {
        for p in loop_points(face, &FACE_OVAL) {
            moves.hold(p);
        }
    }

    // Limbs: both outlines in toward the bone where the person ends there;
    // an outline against something else holds. Joints taper off.
    let arms = unit(sliders.arms) * ARM_STRENGTH;
    let legs = unit(sliders.legs) * LEG_STRENGTH;
    for (kind, from, to) in trunk.segments() {
        let (strength, weight): (f32, fn(f32) -> f32) = match kind {
            LimbKind::UpperArm => (arms, |s| smoothstep(0.0, 0.3, s)),
            LimbKind::Forearm => (arms, |s| 1.0 - smoothstep(0.75, 1.0, s)),
            LimbKind::Thigh => (legs, |s| smoothstep(0.0, 0.3, s)),
            LimbKind::Shin => (legs, |s| 1.0 - smoothstep(0.75, 1.0, s)),
        };
        if strength == 0.0 {
            continue;
        }
        let pieces = (distance(from, to) / step).ceil().clamp(4.0, 40.0) as usize;
        for i in 0..=pieces {
            let s = i as f32 / pieces as f32;
            let Some(e) = trunk.limb_span(from, to, s) else {
                continue;
            };
            moves.hold(e.span.centre);
            for side in 0..2 {
                let edge = e.span.edge(side);
                if e.stops[side] == Stop::Off {
                    moves.shift(
                        edge,
                        sub(e.span.centre, edge).map(|v| v * strength * weight(s)),
                    );
                } else {
                    moves.hold(edge);
                }
            }
        }
    }

    // The rest of the trunk holds: its axis and its outline away from the
    // waist band.
    let band = shape
        .waist_level
        .filter(|_| waist != 0.0)
        .map_or((f32::MAX, f32::MIN), |tw| (tw - 0.3, tw + 0.35));
    for t in Trunk::levels(0.1, 1.2, 0.05) {
        let Some(e) = trunk.span(t) else {
            continue;
        };
        moves.hold(e.span.centre);
        if (t < band.0 || t > band.1) && (t > 0.35 || shoulders == 0.0) {
            moves.hold(e.span.start);
            moves.hold(e.span.end);
        }
    }

    // A ring of anchors around the person.
    let [x0, y0, x1, y1] = body.silhouette.bounds();
    let margin = (0.35 * trunk.shoulder_width).max(16.0);
    let (x0, y0, x1, y1) = (x0 - margin, y0 - margin, x1 + margin, y1 + margin);
    let ring_step = (trunk.torso * 0.1).max(8.0);
    let (nx, ny) = (
        ((x1 - x0) / ring_step).ceil() as usize,
        ((y1 - y0) / ring_step).ceil() as usize,
    );
    let mut ring = Vec::new();
    for i in 0..=nx {
        let x = x0 + (x1 - x0) * i as f32 / nx as f32;
        ring.push([x, y0]);
        ring.push([x, y1]);
    }
    for j in 1..ny {
        let y = y0 + (y1 - y0) * j as f32 / ny as f32;
        ring.push([x0, y]);
        ring.push([x1, y]);
    }
    let mut controls = moves.controls();
    controls.extend(ring.iter().map(|&p| Control::still(p)));
    let fade = margin * 0.5;
    let region = Region::around(ring.into_iter(), [fade; 4], width, height);
    Displacement::from_controls(&controls, region, (trunk.torso / 60.0).max(4.0), fade)
}

/// The leg lengthening: every row below the hips stretched down, the hips
/// and everything above them still, the feet kept inside the image. `None`
/// when the legs are not seen standing.
pub fn leg_field(
    body: &BodyModel,
    sliders: &BodySliders,
    width: u32,
    height: u32,
) -> Option<Displacement> {
    let k = unit(sliders.leg_length).max(0.0) * LEG_LENGTH_STRENGTH;
    if k == 0.0 {
        return None;
    }
    let pose = &body.pose;
    let trunk = Trunk::new(pose, &body.silhouette)?;
    if !trunk.hips_seen {
        return None;
    }
    let hip = mid(pose.at(pose::LEFT_HIP), pose.at(pose::RIGHT_HIP))[1];
    // Standing legs only: thighs roughly down the image.
    let legs = [
        (pose::LEFT_HIP, pose::LEFT_KNEE, pose::LEFT_ANKLE),
        (pose::RIGHT_HIP, pose::RIGHT_KNEE, pose::RIGHT_ANKLE),
    ];
    let mut feet = f32::MIN;
    for (h, knee, ankle) in legs {
        if !pose.seen(knee) {
            continue;
        }
        let (a, b) = (pose.at(h), pose.at(knee));
        let length = distance(a, b).max(1.0);
        if (b[1] - a[1]) / length < 0.8 {
            return None;
        }
        if pose.seen(ankle) {
            feet = feet.max(pose.at(ankle)[1]);
        }
    }
    if feet <= hip {
        return None;
    }
    // Room below the feet (heels a little under the ankles).
    let feet = feet + 0.06 * trunk.torso;
    let room = ((height as f32 - 2.0 - hip) / (feet - hip) - 1.0).max(0.0);
    let k = k.min(room);
    if k <= 0.001 {
        return None;
    }
    // The stretch eases in over a band around the hips: forward, a row at
    // y goes to y + k * ramp(y).
    let ease = 0.12 * trunk.torso;
    let (y0, y1) = (hip - ease, hip + ease);
    let ramp = |y: f32| {
        if y <= y0 {
            0.0
        } else if y <= y1 {
            (y - y0) * (y - y0) / (2.0 * (y1 - y0))
        } else {
            (y1 - y0) * 0.5 + (y - y1)
        }
    };
    let forward = |y: f32| y + k * ramp(y);
    // Inverse, by bisection (forward rises steadily).
    let source = |y: f32| {
        let (mut lo, mut hi) = (y - k * ramp(y) - 1.0, y);
        for _ in 0..30 {
            let m = (lo + hi) * 0.5;
            if forward(m) < y {
                lo = m;
            } else {
                hi = m;
            }
        }
        (lo + hi) * 0.5
    };
    let top = y0.floor().max(0.0) as u32;
    let region = Region {
        x: 0,
        y: top,
        w: width,
        h: height.saturating_sub(top),
    };
    Displacement::from_fn(region, (height as f32 / 300.0).max(4.0), |_, y| {
        [0.0, source(y) - y]
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

    #[test]
    fn moves_at_one_point_add_up() {
        let mut moves = Moves::default();
        moves.shift([10.0, 10.0], [1.0, 0.0]);
        moves.shift([10.4, 9.8], [0.0, 2.0]);
        moves.hold([30.0, 30.0]);
        let controls = moves.controls();
        assert_eq!(controls.len(), 2);
        assert_eq!(controls[0].to, [11.0, 12.0]);
        assert_eq!(controls[1].from, controls[1].to);
    }
}
