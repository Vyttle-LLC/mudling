//! The bundled trigger sounds, played through `rodio` on a thread of its own,
//! so opening the audio device never holds up a frame. With no device they
//! simply don't play.

use rodio::{Decoder, DeviceSinkBuilder};
use std::io::Cursor;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

/// A sound a trigger can ring. `name` is what a trigger saves.
pub struct Sound {
    pub name: &'static str,
    pub label: &'static str,
    bytes: &'static [u8],
}

macro_rules! sound {
    ($name:literal, $label:literal) => {
        Sound {
            name: $name,
            label: $label,
            bytes: include_bytes!(concat!("../../../assets/sounds/", $name, ".wav")),
        }
    };
}

pub const SOUNDS: [Sound; 5] = [
    sound!("bell", "Bell"),
    sound!("chime", "Chime"),
    sound!("alert", "Alert"),
    sound!("low", "Low"),
    sound!("pop", "Pop"),
];

pub fn label_of(name: &str) -> &str {
    SOUNDS
        .iter()
        .find(|sound| sound.name == name)
        .map_or(name, |sound| sound.label)
}

/// Plays sounds until it's dropped.
pub struct Speaker(Sender<&'static [u8]>);

impl Speaker {
    pub fn start() -> Speaker {
        let (cues, heard) = channel();
        let opened = thread::Builder::new()
            .name("mudling-sound".to_owned())
            .spawn(move || play_until_dropped(heard));
        if let Err(error) = opened {
            eprintln!("no sound: {error}");
        }
        Speaker(cues)
    }

    /// Plays the sound called `name`. A name this version doesn't bundle
    /// plays nothing.
    pub fn play(&self, name: &str) {
        if let Some(sound) = SOUNDS.iter().find(|sound| sound.name == name) {
            // The thread only ends without a device: then nothing plays.
            let _ = self.0.send(sound.bytes);
        }
    }
}

fn play_until_dropped(heard: Receiver<&'static [u8]>) {
    let device = match DeviceSinkBuilder::open_default_sink() {
        Ok(device) => device,
        Err(error) => {
            eprintln!("no sound: {error}");
            return;
        }
    };
    for bytes in heard {
        match Decoder::new(Cursor::new(bytes)) {
            Ok(sound) => device.mixer().add(sound),
            Err(error) => eprintln!("a sound didn't decode: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_sound_decodes() {
        for sound in &SOUNDS {
            let decoded = Decoder::new(Cursor::new(sound.bytes));
            assert!(decoded.is_ok(), "{}", sound.name);
        }
    }

    #[test]
    fn a_saved_name_reads_as_its_label_or_as_itself() {
        assert_eq!(label_of("chime"), "Chime");
        assert_eq!(label_of("from-a-newer-version"), "from-a-newer-version");
    }
}
