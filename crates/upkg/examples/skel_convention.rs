//! Research: determines the bone rotation convention empirically. For each convention, computes
//! bone world positions from the reference skeleton and measures how far each bone sits from the
//! centroid of the vertices it most strongly influences. The right convention gives small errors.
//! Also compares animation frame 0 against the reference pose.
//! Usage: skel_convention <file.ukx> [mesh] [anim]

use upkg::Package;
use upkg::skeletal::{read_mesh_animation, read_skeletal_mesh};

type Q = [f32; 4];

fn qmul(a: Q, b: Q) -> Q {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}
fn conj(q: Q) -> Q {
    [-q[0], -q[1], -q[2], q[3]]
}
fn rot(q: Q, v: [f32; 3]) -> [f32; 3] {
    let p = qmul(qmul(q, [v[0], v[1], v[2], 0.0]), conj(q));
    [p[0], p[1], p[2]]
}

fn world(bones: &[( [f32; 4], [f32; 3], usize)], conj_nonroot: bool, conj_root: bool) -> Vec<[f32; 3]> {
    let mut wq: Vec<Q> = vec![];
    let mut wp: Vec<[f32; 3]> = vec![];
    for (i, &(q, p, parent)) in bones.iter().enumerate() {
        let q = if (i == 0 && conj_root) || (i > 0 && conj_nonroot) { conj(q) } else { q };
        if i == 0 {
            wq.push(q);
            wp.push(p);
        } else {
            let pr = rot(wq[parent], p);
            wp.push([wp[parent][0] + pr[0], wp[parent][1] + pr[1], wp[parent][2] + pr[2]]);
            wq.push(qmul(wq[parent], q));
        }
    }
    wp
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let mi = match a.get(1) {
        Some(n) => pkg.find_export(n, Some("SkeletalMesh")).unwrap(),
        None => pkg.exports_of_class("SkeletalMesh").next().unwrap(),
    };
    let m = read_skeletal_mesh(&pkg, mi).unwrap();
    let l = &m.lods[0];
    // Centroid of points per dominant bone.
    let mut best = vec![(0.0f32, usize::MAX); l.points.len()];
    for inf in &l.influences {
        let b = &mut best[inf.point as usize];
        if inf.weight > b.0 {
            *b = (inf.weight, inf.bone as usize);
        }
    }
    let mut sum = vec![([0.0f32; 3], 0usize); m.bones.len()];
    for (p, &(_, b)) in l.points.iter().zip(&best) {
        if b < sum.len() {
            for k in 0..3 {
                sum[b].0[k] += p[k];
            }
            sum[b].1 += 1;
        }
    }
    let bones: Vec<_> = m.bones.iter().map(|b| (b.rotation, b.position, b.parent as usize)).collect();
    for (cn, cr) in [(false, false), (true, false), (true, true), (false, true)] {
        let wp = world(&bones, cn, cr);
        let mut err = 0.0;
        let mut n = 0;
        for (i, (s, c)) in sum.iter().enumerate() {
            if *c > 5 {
                let cen = [s[0] / *c as f32, s[1] / *c as f32, s[2] / *c as f32];
                let d = ((wp[i][0] - cen[0]).powi(2) + (wp[i][1] - cen[1]).powi(2) + (wp[i][2] - cen[2]).powi(2)).sqrt();
                err += d;
                n += 1;
            }
        }
        println!("conj_nonroot={cn} conj_root={cr}: mean bone-to-centroid distance {:.2} over {n} bones", err / n as f32);
    }
    let lo = l.points.iter().fold([f32::MAX; 3], |a, p| [a[0].min(p[0]), a[1].min(p[1]), a[2].min(p[2])]);
    let hi = l.points.iter().fold([f32::MIN; 3], |a, p| [a[0].max(p[0]), a[1].max(p[1]), a[2].max(p[2])]);
    println!("points bbox {lo:?} .. {hi:?}; origin={:?} rot={:?} scale={:?}", m.origin, m.rotation, m.scale);

    if let Some(an) = a.get(2) {
        let ai = pkg.find_export(an, Some("MeshAnimation")).unwrap();
        let anim = read_mesh_animation(&pkg, ai).unwrap();
        let mut diffs = (0.0, 0.0, 0);
        for (s, seq) in anim.sequences.iter().enumerate().take(5) {
            let pose = anim.sample_pose(s, 0.0, false);
            for (i, b) in m.bones.iter().enumerate().take(pose.len()) {
                let (q, p) = pose[i];
                let dot = (0..4).map(|k| q[k] * b.rotation[k]).sum::<f32>().abs();
                diffs.0 += 1.0 - dot;
                diffs.1 += (0..3).map(|k| (p[k] - b.position[k]).powi(2)).sum::<f32>().sqrt();
                diffs.2 += 1;
            }
            println!("seq {} frames={} rate={} track_time={}", seq.name, seq.num_frames, seq.rate, anim.moves[s].track_time);
        }
        println!("anim frame0 vs ref: mean (1-|dot|)={:.4} mean pos diff={:.3}", diffs.0 / diffs.2 as f32, diffs.1 / diffs.2 as f32);
        let names_match = anim.bones.iter().zip(&m.bones).filter(|(a, b)| a.name.eq_ignore_ascii_case(&b.name)).count();
        println!("bone names matching by index: {names_match}/{}", m.bones.len());
    }
}
