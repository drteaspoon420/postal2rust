//! Lists save slots like the Load Game menu, plus what each `.usa` says about itself.
//!
//! `p2save [--game dir] [--save-dir dir --ini file]`

use p2save::{list_slots, read_save_summary, save_dir, slot_ini_path};
use std::path::PathBuf;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let game = PathBuf::from(get("--game").unwrap_or_else(|| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into()));
    let dir = get("--save-dir").map(PathBuf::from).unwrap_or_else(|| save_dir(&game));
    let ini = get("--ini").map(PathBuf::from).unwrap_or_else(|| slot_ini_path(&game));
    let mut slots: Vec<_> = list_slots(&dir, &ini).into_iter().filter(|s| s.exists).collect();
    slots.sort_by(|a, b| b.sort_time().cmp(&a.sort_time()));
    let (mut ok, mut agree) = (0, 0);
    for s in &slots {
        let t0 = std::time::Instant::now();
        match read_save_summary(&s.path) {
            Ok(sum) => {
                ok += 1;
                // The ini name starts with the day name; check it against the package.
                let consistent = s.info.name.is_empty() || s.info.name.starts_with(sum.day_name());
                agree += consistent as usize;
                println!(
                    "{:>10} | {:<62} | {:<18} | usa: {} - {} - diff {:?} - played {:.0} min - kills {:?} - {} / {} ({} exports, {:.1} MB, {} ms){}",
                    s.label(),
                    s.display_name(),
                    s.info.time,
                    sum.day_name(),
                    sum.level_title,
                    sum.difficulty,
                    sum.time_elapsed.unwrap_or(0.0) / 60.0,
                    sum.people_killed,
                    sum.game_info_class,
                    sum.game_state_class,
                    sum.exports,
                    s.file_size as f64 / 1e6,
                    t0.elapsed().as_millis(),
                    if consistent { "" } else { "  DAY MISMATCH" }
                );
            }
            Err(e) => println!("{:>10} | {} | ERROR {e}", s.label(), s.path.display()),
        }
    }
    println!("SAVES existing={} parsed={ok} day_agrees_with_ini={agree}", slots.len());
}
