use std::collections::VecDeque;

use log::info;

use crate::youtube_dl::AudioMetadata;

pub struct Playlist {
    data: VecDeque<AudioMetadata>,
}

impl Playlist {
    pub fn new() -> Self {
        Self {
            data: VecDeque::new(),
        }
    }

    pub fn push(&mut self, data: AudioMetadata) {
        info!("Adding {:?} to playlist", &data.title);

        self.data.push_front(data)
    }

    pub fn pop(&mut self) -> Option<AudioMetadata> {
        let res = self.data.pop_back();
        info!("Popping {:?} from playlist", res.as_ref().map(|r| &r.title));

        res
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn clear(&mut self) {
        self.data.clear();

        info!("Cleared playlist")
    }
}
