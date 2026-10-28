//! Choosing the subject among detections, frame after frame.

use crate::Detection;

/// What a sampled frame decided.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Subject {
    /// Point to centre the framing on, in picture pixels.
    pub x: f64,
    pub y: f64,
    /// Box of the person (or face when no person was found).
    pub bbox: Detection,
    /// A face sits in the box: the point is the face centre.
    pub has_face: bool,
    pub score: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefer {
    /// The biggest, most confident subject first, then the nearest to the previous one.
    Auto,
    /// Faces are the subject; persons without a face are ignored.
    Face,
}

fn inside(face: &Detection, person: &Detection) -> bool {
    let (cx, cy) = face.center();
    cx >= person.x0 && cx <= person.x1 && cy >= person.y0 && cy <= person.y1
}

/// The point to frame on for a person: the face centre when a face is inside the box, else a
/// point a quarter of the way down the box (head and shoulders).
fn aim(person: &Detection, faces: &[Detection]) -> (f64, f64, bool) {
    let face = faces.iter().filter(|f| inside(f, person)).max_by(|a, b| a.area().total_cmp(&b.area()));
    match face {
        Some(f) => {
            let (cx, cy) = f.center();
            (f64::from(cx), f64::from(cy), true)
        }
        None => (f64::from((person.x0 + person.x1) * 0.5), f64::from(person.y0 + person.height() * 0.25), false),
    }
}

/// Picks the subject of one frame. A candidate's rank is its area and score, minus its distance
/// to the previous subject (in box heights) once there is one, so the framing sticks to the same
/// person instead of jumping to a bigger passer-by.
pub fn pick(prev: Option<&Subject>, persons: &[Detection], faces: &[Detection], prefer: Prefer) -> Option<Subject> {
    let mut candidates: Vec<(Detection, Option<Detection>)> = Vec::new();
    for p in persons {
        if prefer == Prefer::Face && !faces.iter().any(|f| inside(f, p)) {
            continue;
        }
        candidates.push((*p, None));
    }
    // faces with no person box around them (head and shoulders shots) stand on their own
    for f in faces {
        if !persons.iter().any(|p| inside(f, p)) {
            candidates.push((*f, Some(*f)));
        }
    }
    let rank = |c: &(Detection, Option<Detection>)| -> f64 {
        let b = &c.0;
        let base = f64::from(b.area()).sqrt() * f64::from(b.score);
        match prev {
            Some(p) => {
                let (cx, cy) = b.center();
                let dist = ((f64::from(cx) - (f64::from(p.bbox.x0) + f64::from(p.bbox.x1)) * 0.5).powi(2)
                    + (f64::from(cy) - (f64::from(p.bbox.y0) + f64::from(p.bbox.y1)) * 0.5).powi(2))
                .sqrt();
                let scale = f64::from(p.bbox.height()).max(1.0);
                base * (1.0 + 2.0 * f64::from(b.iou(&p.bbox))) / (1.0 + dist / scale)
            }
            None => base,
        }
    };
    let mut best = candidates.iter().max_by(|a, b| rank(a).total_cmp(&rank(b)))?;
    // hysteresis: the incumbent (the candidate that overlaps or sits closest to the previous
    // subject) is only dropped for one that ranks more than twice as high
    if let Some(p) = prev {
        let near = |c: &(Detection, Option<Detection>)| {
            let (cx, cy) = c.0.center();
            ((f64::from(cx) - (f64::from(p.bbox.x0) + f64::from(p.bbox.x1)) * 0.5).powi(2)
                + (f64::from(cy) - (f64::from(p.bbox.y0) + f64::from(p.bbox.y1)) * 0.5).powi(2))
            .sqrt()
                / f64::from(p.bbox.height()).max(1.0)
        };
        if let Some(inc) = candidates.iter().filter(|c| c.0.iou(&p.bbox) > 0.05 || near(c) < 1.0).min_by(|a, b| near(a).total_cmp(&near(b)))
            && rank(inc) * 2.0 >= rank(best)
        {
            best = inc;
        }
    }
    let (x, y, has_face) = match best.1 {
        Some(f) => {
            let (cx, cy) = f.center();
            (f64::from(cx), f64::from(cy), true)
        }
        None => aim(&best.0, faces),
    };
    Some(Subject { x, y, bbox: best.0, has_face, score: best.0.score })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(x0: f32, y0: f32, x1: f32, y1: f32, s: f32) -> Detection {
        Detection { x0, y0, x1, y1, score: s }
    }

    #[test]
    fn aims_at_the_face_else_the_upper_body() {
        let person = d(100.0, 0.0, 200.0, 400.0, 0.9);
        let face = d(130.0, 20.0, 170.0, 70.0, 0.9);
        let s = pick(None, &[person], &[face], Prefer::Auto).expect("subject");
        assert!(s.has_face && (s.x - 150.0).abs() < 1e-6 && (s.y - 45.0).abs() < 1e-6);
        let s = pick(None, &[person], &[], Prefer::Auto).expect("subject");
        assert!(!s.has_face && (s.y - 100.0).abs() < 1e-6);
        assert!(pick(None, &[person], &[], Prefer::Face).is_none());
    }

    #[test]
    fn sticks_to_the_previous_subject() {
        let a = d(100.0, 100.0, 200.0, 400.0, 0.8);
        let bigger = d(600.0, 50.0, 800.0, 450.0, 0.9);
        let first = pick(None, &[a], &[], Prefer::Auto).expect("subject");
        let next = pick(Some(&first), &[bigger, d(104.0, 100.0, 204.0, 400.0, 0.7)], &[], Prefer::Auto).expect("subject");
        assert!(next.x < 300.0, "kept the first person, got x = {}", next.x);
        // with nothing to stick to, the bigger one wins
        assert!(pick(None, &[a, bigger], &[], Prefer::Auto).map(|s| s.x).unwrap_or(0.0) > 600.0);
    }
}
