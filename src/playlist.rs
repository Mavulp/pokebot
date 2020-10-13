use std::collections::VecDeque;

use slog::{info, Logger};

use crate::youtube_dl::AudioMetadata;

pub struct Playlist {
    data: VecDeque<AudioMetadata>,
    logger: Logger,
}

impl Playlist {
    pub fn new(logger: Logger) -> Self {
        Self {
            data: VecDeque::new(),
            logger,
        }
    }

    pub fn push(&mut self, data: AudioMetadata) {
        info!(self.logger, "Adding to playlist"; "title" => &data.title);

        self.data.push_front(data)
    }

    pub fn pop(&mut self) -> Option<AudioMetadata> {
        let res = self.data.pop_back();
        info!(
            self.logger,
            "Popping from playlist";
            "title" => res.as_ref().map(|r| &r.title)
        );

        res
    }

    pub fn to_vec(&self) -> Vec<AudioMetadata> {
        let (a, b) = self.data.as_slices();

        let mut res = a.to_vec();
        res.extend_from_slice(b);
        res.reverse();

        res
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn clear(&mut self) {
        self.data.clear();

        info!(self.logger, "Cleared playlist")
    }
}
