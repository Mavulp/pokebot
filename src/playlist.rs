use std::collections::VecDeque;

use log::info;

pub struct Playlist {
    data: VecDeque<AudioRequest>,
}

impl Playlist {
    pub fn new() -> Self {
        Self {
            data: VecDeque::new(),
        }
    }

    pub fn push(&mut self, req: AudioRequest) {
        info!("Adding {} to playlist", &req.title);

        self.data.push_front(req)
    }

    pub fn pop(&mut self) -> Option<AudioRequest> {
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

#[derive(Clone, Debug)]
pub struct AudioRequest {
    pub title: String,
    pub address: String,
}
