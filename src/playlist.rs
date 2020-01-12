use log::info;

pub struct Playlist {
    data: Vec<Option<AudioRequest>>,
    read: usize,
    write: usize,
    is_full: bool,
}

impl Playlist {
    pub fn new() -> Self {
        Self {
            data: Vec::with_capacity(50),
            read: 0,
            write: 0,
            is_full: false,
        }
    }

    pub fn push(&mut self, req: AudioRequest) -> bool {
        if self.is_full {
            return false;
        }

        info!("Adding {} to playlist", &req.title);

        if self.data.len() < self.data.capacity() {
            self.data.push(Some(req));
        } else {
            self.data[self.write] = Some(req);
        }

        self.write = (self.write + 1) % self.data.capacity();

        if self.write == self.read {
            self.is_full = true;
        }


        true
    }

    pub fn is_empty(&self) -> bool {
        !self.is_full && self.write == self.read
    }

    pub fn is_full(&self) -> bool {
        self.is_full
    }

    pub fn pop(&mut self) -> Option<AudioRequest> {
        if self.is_empty() {
            None
        } else {
            self.is_full = false;
            let res = self.data[self.read].take();
            self.read += 1;

            info!("Popping {:?} from playlist", res.as_ref().map(|r| &r.title));

            res
        }
    }

    pub fn clear(&mut self) {
        self.data.clear();
        self.read = 0;
        self.write = 0;
        self.is_full = false;

        info!("Cleared playlist")
    }
}

#[derive(Clone, Debug)]
pub struct AudioRequest {
    pub title: String,
    pub address: String,
}
