//! Scales and the nearest legal note. Masks follow HelixTune's table: bit s set means the
//! semitone s above the root is a scale degree.

pub const SCALES: [(&str, u16); 10] = [
    ("Chromatic", 0xFFF),
    ("Major", 0xAB5),
    ("Minor", 0x5AD),
    ("Harmonic Minor", 0x9AD),
    ("Dorian", 0x6AD),
    ("Phrygian", 0x5AB),
    ("Mixolydian", 0x6B5),
    ("Major Pentatonic", 0x295),
    ("Minor Pentatonic", 0x4A9),
    ("Blues", 0x4E9),
];

pub const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// The nearest pitch of `root` + `mask` to a fractional MIDI note, or None if the scale is empty.
pub fn nearest(midi: f32, root: i32, mask: u16) -> Option<f32> {
    if !midi.is_finite() || mask & 0xFFF == 0 {
        return None;
    }
    let root = root.rem_euclid(12);
    let centre = ((midi - root as f32) / 12.0).floor() as i32;
    let mut best: Option<(f32, f32)> = None;
    // The input's octave plus its neighbours covers even a six-semitone gap.
    for k in centre - 1..=centre + 1 {
        for s in 0..12 {
            if mask >> s & 1 == 0 {
                continue;
            }
            let cand = (root + 12 * k + s) as f32;
            let d = (cand - midi).abs();
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, cand));
            }
        }
    }
    best.map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_to_the_scale() {
        let major = SCALES[1].1;
        assert_eq!(nearest(60.9, 0, major), Some(60.0)); // a sharp C in C major -> C
        assert_eq!(nearest(61.6, 0, major), Some(62.0)); // closer to D
        assert_eq!(nearest(66.4, 2, major), Some(66.0)); // F# is in D major
        let minor = SCALES[2].1;
        assert_eq!(nearest(64.2, 9, minor), Some(64.0)); // E in A minor
        assert_eq!(nearest(60.4, 0, SCALES[0].1), Some(60.0));
    }
}
