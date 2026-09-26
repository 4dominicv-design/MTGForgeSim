//! GENERATED (model-export) — tag map + policy keep-bars for the learned mulligan modes.
#![allow(clippy::all)]
pub const N_TAGS: usize = 20;

/// Accumulate a card's property tags into the tag vector (unknown cards contribute nothing).
pub fn add_card_tags(name: &str, t: &mut [f32]) {
    match name {
        "Underground Sea" => { t[0] += 1.0; t[1] += 1.0; t[2] += 1.0; t[3] += 1.0; t[5] += 1.0; }
        "Swamp" => { t[0] += 1.0; t[2] += 1.0; t[3] += 1.0; t[5] += 1.0; }
        "Island" => { t[0] += 1.0; t[1] += 1.0; t[3] += 1.0; t[5] += 1.0; }
        "Undercity Sewers" => { t[0] += 1.0; t[1] += 1.0; t[2] += 1.0; t[5] += 1.0; t[11] += 1.0; t[8] += 1.0; }
        "Polluted Delta" => { t[0] += 1.0; t[1] += 1.0; t[2] += 1.0; t[3] += 1.0; t[4] += 1.0; t[5] += 1.0; t[11] += 1.0; t[8] += 1.0; }
        "Flooded Strand" => { t[0] += 1.0; t[1] += 1.0; t[2] += 1.0; t[3] += 1.0; t[4] += 1.0; t[5] += 1.0; t[11] += 1.0; t[8] += 1.0; }
        "Misty Rainforest" => { t[0] += 1.0; t[1] += 1.0; t[2] += 1.0; t[3] += 1.0; t[4] += 1.0; t[5] += 1.0; t[11] += 1.0; t[8] += 1.0; }
        "Scalding Tarn" => { t[0] += 1.0; t[1] += 1.0; t[2] += 1.0; t[3] += 1.0; t[4] += 1.0; t[5] += 1.0; t[11] += 1.0; t[8] += 1.0; }
        "Bloodstained Mire" => { t[0] += 1.0; t[1] += 1.0; t[2] += 1.0; t[3] += 1.0; t[4] += 1.0; t[5] += 1.0; t[11] += 1.0; t[8] += 1.0; }
        "Lotus Petal" => { t[5] += 1.0; t[6] += 1.0; t[8] += 1.0; }
        "Dark Ritual" => { t[7] += 1.0; t[9] += 1.0; }
        "Ponder" => { t[14] += 1.0; t[9] += 1.0; }
        "Brainstorm" => { t[13] += 1.0; t[9] += 1.0; }
        "Consider" => { t[12] += 1.0; t[9] += 1.0; }
        "Flow State" => { t[13] += 1.0; t[10] += 1.0; }
        "Street Wraith" => { t[11] += 1.0; t[8] += 1.0; }
        "Doomsday" => { t[15] += 1.0; }
        "Force of Will" => { t[16] += 1.0; }
        "Daze" => { t[16] += 1.0; }
        "Thoughtseize" => { t[16] += 1.0; }
        "Murktide Regent" => { t[17] += 1.0; }
        "Wasteland" => { t[17] += 1.0; t[16] += 1.0; }
        "Tamiyo, Inquisitive Student" => { t[18] += 1.0; }
        "Cavern of Souls" => { t[19] += 1.0; }
        "Thassa's Oracle" => { t[19] += 1.0; }
        "Jace, Wielder of Mysteries" => { t[19] += 1.0; }
        "Edge of Autumn" => { t[19] += 1.0; }
        "Lion's Eye Diamond" => { t[19] += 1.0; }
        _ => {}
    }
}

/// Blue cards usable as a Force-of-Will pitch (for FoW deployability).
pub fn is_blue(name: &str) -> bool { matches!(name, "Force of Will" | "Brainstorm" | "Ponder" | "Consider" | "Flow State" | "Daze" | "Murktide Regent" | "Tamiyo, Inquisitive Student" | "Jace, Wielder of Mysteries") }

/// keep-bar by hand size [7,6,5,4,3,2]: keep iff best-subset score >= bar.
pub const BARS_PLAY_SPEED: [f32; 6] = [0.7127, 0.6345, 0.5303, 0.3865, 0.1868, 0.0292];
/// keep-bar by hand size [7,6,5,4,3,2]: keep iff best-subset score >= bar.
pub const BARS_DRAW_SPEED: [f32; 6] = [0.784, 0.718, 0.626, 0.497, 0.316, 0.099];
/// keep-bar by hand size [7,6,5,4,3,2]: keep iff best-subset score >= bar.
pub const BARS_PLAY_WIN: [f32; 6] = [0.424, 0.344, 0.241, 0.131, 0.057, 0.009];
/// keep-bar by hand size [7,6,5,4,3,2]: keep iff best-subset score >= bar.
pub const BARS_DRAW_WIN: [f32; 6] = [0.4848, 0.4099, 0.3106, 0.1961, 0.1038, 0.0382];
